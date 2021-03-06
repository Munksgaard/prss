use std::process::Command;

use anyhow::{Context, Result};
use atom_syndication as atom;
use chrono::{DateTime, FixedOffset};
use std::fs::File;
use std::io;
use std::io::{BufRead, BufReader};
use termion::event::Key;
use termion::input::TermRead;
use termion::raw::IntoRawMode;
use termion::screen::AlternateScreen;
use tui::backend::TermionBackend;
use tui::layout::Margin;
use tui::style::{Color, Style};
use tui::widgets::{Block, Borders, List, ListItem, ListState};
use tui::Terminal;

enum Entry {
    Atom(Box<atom::Entry>),
    Rss(Box<rss::Item>),
}

impl Entry {
    fn title(&self) -> Option<&str> {
        use Entry::*;

        match self {
            Atom(entry) => Some(&entry.title),
            Rss(item) => item.title(),
        }
    }

    fn date(&self) -> Option<DateTime<FixedOffset>> {
        use Entry::*;

        match self {
            Atom(entry) => entry.published,
            Rss(item) => item
                .pub_date
                .as_ref()
                .and_then(|d| chrono::DateTime::parse_from_rfc2822(d).ok()),
        }
    }

    fn link(&self) -> Option<&str> {
        use Entry::*;

        match self {
            Atom(entry) => entry.links.first().map(|x| x.href.as_ref()),
            Rss(item) => item.link.as_ref().map(|x| x.as_ref()),
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

#[tokio::main]
async fn main() -> Result<()> {
    let xdg_dirs = xdg::BaseDirectories::with_prefix("prss")?;
    let feeds_txt = xdg_dirs
        .place_config_file("feeds.txt")
        .expect("cannot create configuration directory");
    let feeds_txt = File::open(feeds_txt).context("feeds.txt")?;
    let feed_urls: Vec<String> = BufReader::new(feeds_txt)
        .lines()
        .map(|l| l.unwrap())
        .collect();

    let screen = AlternateScreen::from(io::stdout().into_raw_mode()?);
    let stdin = io::stdin();
    let backend = TermionBackend::new(screen);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut entries = Vec::new();

    for url in feed_urls {
        let content = reqwest::get(&url).await?.bytes().await?;

        let mut res: Vec<_> = if let Ok(feed) = atom::Feed::read_from(&content[..]) {
            feed.entries
                .into_iter()
                .map(|e| Entry::Atom(Box::new(e)))
                .collect()
        } else if let Ok(channel) = rss::Channel::read_from(&content[..]) {
            channel
                .items
                .into_iter()
                .map(|i| Entry::Rss(Box::new(i)))
                .collect()
        } else {
            panic!("Couldn't read Atom or RSS from input.")
        };

        entries.append(&mut res)
    }

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
                .map(|i| ListItem::new(i.title().unwrap_or("")))
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
