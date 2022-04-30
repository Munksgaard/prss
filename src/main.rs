use std::fs::{metadata, File};
use std::io;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use atom_syndication as atom;
use chrono::{DateTime, FixedOffset};
use futures::StreamExt;
use termion::event::Key;
use termion::input::TermRead;
use termion::raw::IntoRawMode;
use termion::screen::AlternateScreen;
use tui::backend::TermionBackend;
use tui::layout::Margin;
use tui::style::{Color, Style};
use tui::widgets::{Block, Borders, List, ListItem, ListState};
use tui::Terminal;

#[derive(Clone)]
enum Entry {
    Atom(String, Box<atom::Entry>),
    Rss(String, Box<rss::Item>),
}

impl Entry {
    fn title(&self) -> Option<String> {
        use Entry::*;

        match self {
            Atom(source, entry) => Some(format!("{} ({})", entry.title, source)),
            Rss(source, item) => item.title().map(|t| format!("{} ({})", t, source)),
        }
    }

    fn date(&self) -> Option<DateTime<FixedOffset>> {
        use Entry::*;

        match self {
            Atom(_, entry) => entry.published,
            Rss(_, item) => item
                .pub_date
                .as_ref()
                .and_then(|d| chrono::DateTime::parse_from_rfc2822(d).ok()),
        }
    }

    fn link(&self) -> Option<&str> {
        use Entry::*;

        match self {
            Atom(_, entry) => entry.links.first().map(|x| x.href.as_ref()),
            Rss(_, item) => item.link.as_ref().map(|x| x.as_ref()),
        }
    }
}

struct FeedList<T> {
    items: Vec<T>,
    state: ListState,
}

impl<T> FeedList<T> {
    fn new(items: Vec<T>) -> FeedList<T> {
        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(0));
        }

        FeedList { items, state }
    }

    pub fn next(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i >= self.items.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    pub fn previous(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i == 0 {
                    self.items.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    pub fn get(&self) -> &T {
        &self.items[self.state.selected().expect("impossible")]
    }
}

fn read_feed(content: &[u8]) -> Result<Vec<Entry>> {
    if let Ok(feed) = atom::Feed::read_from(content) {
        let t = feed.title.clone();
        Ok(feed
            .entries
            .into_iter()
            .map(move |e| Entry::Atom(t.clone(), Box::new(e)))
            .collect())
    } else if let Ok(channel) = rss::Channel::read_from(content) {
        let t = channel.title.clone();
        Ok(channel
            .items
            .into_iter()
            .map(move |i| Entry::Rss(t.clone(), Box::new(i)))
            .collect())
    } else {
        bail!("Couldn't read Atom or RSS from input.")
    }
}

async fn get_feed_entries(client: &reqwest::Client, url: &str) -> Result<Vec<Entry>> {
    let digest = md5::compute(url);
    let xdg_dirs = xdg::BaseDirectories::with_prefix("prss")?;
    let cache_file = xdg_dirs.find_cache_file(format!("{:x}", digest));
    let response = client.head(url).send().await?;
    match (
        cache_file
            .ok_or_else(|| anyhow!("Cachefile not found"))
            .and_then(|x| Ok((x.clone(), metadata(x).context("metadata")?)))
            .and_then(|(y, x)| Ok((y, x.modified().context("modified")?))),
        response
            .headers()
            .get(reqwest::header::LAST_MODIFIED)
            .ok_or_else(|| anyhow!("No last_modified header found"))
            .and_then(|x| x.to_str().context("to_str"))
            .and_then(|x| DateTime::parse_from_rfc2822(x).context("parse_from_rfc2822")),
    ) {
        (Ok((cache, file_last_modified)), Ok(url_last_modified))
            if file_last_modified >= std::convert::From::from(url_last_modified) =>
        {
            let mut handle = File::open(cache).context("open")?;
            let mut buf = vec![];
            handle.read_to_end(&mut buf)?;
            read_feed(&buf[..])
        }
        _ => {
            let content = reqwest::get(url).await?.bytes().await?;
            let feed = read_feed(&content[..]);
            let path = xdg_dirs.place_cache_file(format!("{:x}", digest))?;
            let mut f = File::create(path)?;
            f.write_all(&content[..])?;
            feed
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let xdg_dirs = xdg::BaseDirectories::with_prefix("prss")?;
    let feeds_txt = xdg_dirs
        .place_config_file("feeds.txt")
        .expect("cannot create configuration directory");
    let feeds_txt = File::open(feeds_txt).context("feeds.txt")?;
    let feed_urls = BufReader::new(feeds_txt).lines().map(|l| l.unwrap());

    let screen = AlternateScreen::from(io::stdout().into_raw_mode()?);
    let stdin = io::stdin();
    let backend = TermionBackend::new(screen);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let client = reqwest::Client::new();

    let fetches = futures::stream::iter(feed_urls.map(|url| {
        let client = client.clone();
        async move { get_feed_entries(&client, &url).await }
    }))
    .buffer_unordered(8)
    .collect::<Vec<_>>()
    .await;
    let mut entries = fetches
        .into_iter()
        .collect::<Result<Vec<Vec<Entry>>>>()?
        .concat();

    entries.sort_by_key(|x| x.date());
    entries.reverse();

    let mut events = stdin.keys();

    let mut feedlist = FeedList::new(entries);
    loop {
        terminal.draw(|f| {
            let rect = f.size().inner(&Margin {
                vertical: 1,
                horizontal: 1,
            });

            let items: Vec<ListItem> = feedlist
                .items
                .iter()
                .map(|i| ListItem::new(i.title().unwrap_or_else(|| String::from(""))))
                .collect();

            let items = List::new(items)
                .block(Block::default().title("Feed Entries").borders(Borders::ALL))
                .style(Style::default().fg(Color::White))
                .highlight_style(Style::default().bg(Color::White).fg(Color::Black))
                .highlight_symbol("> ");

            f.render_stateful_widget(items, rect, &mut feedlist.state);
        })?;

        match events.next() {
            Some(Ok(Key::Char('q'))) => break,
            Some(Ok(Key::Down)) | Some(Ok(Key::Char('j'))) | Some(Ok(Key::Char('n'))) => {
                feedlist.next();
            }
            Some(Ok(Key::Up)) | Some(Ok(Key::Char('k'))) | Some(Ok(Key::Char('p'))) => {
                feedlist.previous();
            }
            Some(Ok(Key::Char('\n'))) => {
                if let Some(url) = feedlist.get().link() {
                    Command::new("xdg-open")
                        .arg(url)
                        .status()
                        .unwrap_or_else(|e| panic!("Failed to open link: {}", e));
                }
            }
            Some(Ok(Key::Ctrl('c'))) => break,
            _ => {}
        }
    }
    Ok(())
}
