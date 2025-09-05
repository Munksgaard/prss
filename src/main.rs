use std::collections::HashSet;
use std::fs::{metadata, File};
use std::io;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use atom_syndication as atom;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use itertools::process_results;
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
struct FeedEntry {
    title: String,
    url: String,
    date: DateTime<Utc>,
}

struct Feed {
    title: String,
    entries: Vec<FeedEntry>,
}

impl Feed {
    fn list_entries(&self) -> Vec<FeedListEntry> {
        self.entries
            .iter()
            .map(|e| FeedListEntry {
                title: format!("{} ({})", e.title, self.title),
                url: e.url.clone(),
                date: e.date,
            })
            .collect()
    }
}

#[derive(Clone)]
struct FeedListEntry {
    title: String,
    url: String,
    date: DateTime<Utc>,
}

struct FeedList {
    items: Vec<FeedListEntry>,
    state: ListState,
}

impl FeedList {
    fn new(items: Vec<Feed>) -> FeedList {
        let mut items = items
            .iter()
            .map(|e| e.list_entries())
            .collect::<Vec<Vec<_>>>()
            .concat();

        items.sort_by_key(|x| x.date);
        items.reverse();

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

    pub fn get(&self) -> &FeedListEntry {
        &self.items[self.state.selected().expect("impossible")]
    }
}

fn read_feed(url: &str, content: &[u8]) -> Result<Feed> {
    if let Ok(feed) = atom::Feed::read_from(content) {
        Ok(Feed {
            title: feed.title().to_string(),
            entries: feed
                .entries
                .into_iter()
                .map(move |e| FeedEntry {
                    title: e.title().to_string(),
                    url: e.links().first().unwrap().href.clone(),
                    date: DateTime::<Utc>::from(e.published.unwrap()),
                })
                .collect(),
        })
    } else if let Ok(channel) = rss::Channel::read_from(content) {
        let t = channel.title.clone();
        Ok(Feed {
            title: channel.title.clone(),
            entries: channel
                .items
                .into_iter()
                .map(move |i| FeedEntry {
                    title: i.title().unwrap_or("").to_string(),
                    url: i.link().unwrap().to_string(),
                    date: DateTime::<Utc>::from(
                        i.pub_date
                            .as_ref()
                            .and_then(|d| {
                                chrono::DateTime::parse_from_rfc2822(&d.replace("UTC", "+0000"))
                                    .ok()
                            })
                            .unwrap_or_else(|| {
                                panic!(
                                    "title: {}, url: {}: couldn't parse i.pub_date {:?}",
                                    t.clone(),
                                    url,
                                    i.pub_date.map(|x| x.replace("UTC", "GMT"))
                                )
                            }),
                    ),
                })
                .collect(),
        })
    } else {
        bail!("Couldn't read Atom or RSS from url: {}", url)
    }
}

fn get_read_entries(xdg_dirs: &xdg::BaseDirectories) -> Result<HashSet<String>> {
    if let Some(entries_file) = xdg_dirs.find_cache_file("read_entries.txt".to_string()) {
        let reader = BufReader::new(File::open(entries_file).context("open")?);
        process_results(reader.lines(), |lines| lines.collect()).context("lines")
    } else {
        Ok(HashSet::new())
    }
}

async fn get_feed_entries(
    client: &reqwest::Client,
    xdg_dirs: &xdg::BaseDirectories,
    url: &str,
) -> Result<Feed> {
    let digest = md5::compute(url);
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
            read_feed(url, &buf[..])
        }
        _ => {
            let content = reqwest::get(url).await?.bytes().await?;
            let feed = read_feed(url, &content[..]);
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
    let feed_urls: Vec<String> = process_results(BufReader::new(feeds_txt).lines(), |lines| {
        lines.filter(|line| !line.starts_with('#')).collect()
    })?;

    let client = reqwest::Client::new();

    let fetches = futures::stream::iter(feed_urls.iter().map(|url| {
        let client = client.clone();
        let xdg_dirs = xdg_dirs.clone();
        async move { get_feed_entries(&client, &xdg_dirs, url).await }
    }))
    .buffer_unordered(8)
    .collect::<Vec<_>>()
    .await;
    let entries = fetches.into_iter().collect::<Result<Vec<Feed>>>()?;

    let mut read_entries = get_read_entries(&xdg_dirs)?;

    let screen = AlternateScreen::from(io::stdout().into_raw_mode()?);
    let stdin = io::stdin();
    let backend = TermionBackend::new(screen);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

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
                .filter(|i| !read_entries.contains(&i.url))
                .map(|i| ListItem::new(i.title.clone()))
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
                let url = feedlist.get().url.clone();
                Command::new("xdg-open")
                    .arg(url)
                    .status()
                    .unwrap_or_else(|e| panic!("Failed to open link: {}", e));
            }
            Some(Ok(Key::Char('r'))) => {
                read_entries.insert(feedlist.get().url.clone());
                let mut file = if let Some(entries_file) =
                    xdg_dirs.find_cache_file("read_entries.txt".to_string())
                {
                    use std::fs::OpenOptions;

                    OpenOptions::new().append(true).open(entries_file)?
                } else {
                    File::create(xdg_dirs.place_cache_file("read_entries.txt".to_string())?)?
                };
                writeln!(file, "{}", &feedlist.get().url)?;
            }
            Some(Ok(Key::Ctrl('c'))) => break,
            _ => {}
        }
    }
    Ok(())
}
