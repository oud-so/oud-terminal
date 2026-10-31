//! Interface state and everything a key press can do to it.
//!
//! Network calls never run on this thread: each one is handed to a worker
//! that posts a [`Msg`] back, so the screen keeps drawing while things load.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::daypart::{self, Daypart};
use crate::api::{Api, Country, DeviceCode, DevicePoll, FeaturedList, Genre, Station, User, UserList};
use crate::audio::{Event, Player};
use crate::map::{self, Map};
use crate::spectrum::Spectrum;
use crate::store::{Store, session};
use crate::visual::{Pattern, Visual};

const VOLUME_STEP: f32 = 0.05;
/// Earlier track titles kept for the details panel.
const HISTORY: usize = 3;
pub const SIGNAL_COUNT: u32 = 8;

/// Something that arrives later.
pub enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Load<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            Load::Ready(v) => Some(v),
            _ => None,
        }
    }
}

impl<T> From<anyhow::Result<T>> for Load<T> {
    fn from(r: anyhow::Result<T>) -> Self {
        match r {
            Ok(v) => Load::Ready(v),
            Err(e) => Load::Failed(e.to_string()),
        }
    }
}

pub enum Msg {
    Featured(anyhow::Result<Vec<FeaturedList>>),
    Signals(anyhow::Result<Vec<Station>>),
    List(String, anyhow::Result<Vec<Station>>),
    Stations(u64, anyhow::Result<Vec<Station>>),
    Genres(anyhow::Result<Vec<Genre>>),
    Countries(anyhow::Result<Vec<Country>>),
    Something(anyhow::Result<(String, Vec<Station>)>),
    /// Who the stored token belongs to.
    Session(anyhow::Result<Option<User>>),
    /// A browser sign-in got its code, for the attempt numbered.
    DeviceCode(u64, anyhow::Result<DeviceCode>),
    /// A sign-in attempt ended: with a session token, or why not.
    SignedIn(u64, anyhow::Result<String>),
    CodeSent(u64, anyhow::Result<()>),
    Bookmarks(anyhow::Result<Vec<Station>>),
    Lists(anyhow::Result<Vec<UserList>>),
    UserList(String, anyhow::Result<Vec<Station>>),
    /// A bookmark change the server refused.
    BookmarkFailed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Explore,
    Stations,
    Library,
    /// Keys, and where oud lives.
    About,
    /// The account: signing in, or who is signed in. Reached from the top right.
    You,
}

pub const SCREENS: [(Screen, &str); 4] = [
    (Screen::Explore, "explore"),
    (Screen::Stations, "stations"),
    (Screen::Library, "library"),
    (Screen::About, "about"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ExploreTab {
    Featured,
    Signals,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LibraryTab {
    Bookmarks,
    Lists,
}

/// Whether anyone is signed in.
pub enum Account {
    /// A token was found and is being checked.
    Checking,
    SignedOut,
    SignedIn(User),
    /// A token is kept but the server could not be asked about it.
    Unreachable(String),
}

/// How a sign-in is going, while signed out.
pub enum SignIn {
    /// Picking a way in.
    Choose,
    /// Approving a code in the browser.
    Browser {
        code: Option<DeviceCode>,
        failed: Option<String>,
        cancel: Arc<AtomicBool>,
    },
    Email(EmailSignIn),
}

#[derive(Default)]
pub struct EmailSignIn {
    pub email: String,
    pub code: String,
    /// The code is on its way; now it is being typed.
    pub sent: bool,
    pub busy: bool,
    pub error: Option<String>,
}

/// Ways in, in the order they are listed.
pub const SIGN_IN_WAYS: [(&str, &str); 2] = [("continue in the browser", "b"), ("email me a code", "m")];

/// One of the account's lists, opened from the library.
pub struct OpenUserList {
    pub list: UserList,
    pub stations: Load<Vec<Station>>,
    pub sel: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    Genre,
    Country,
}

pub struct Picker {
    pub kind: PickerKind,
    pub filter: String,
    pub sel: usize,
}

/// A featured list opened from explore.
pub struct OpenList {
    pub list: FeaturedList,
    pub stations: Load<Vec<Station>>,
    pub sel: usize,
}

#[derive(Clone, PartialEq)]
pub enum Signal {
    Connecting,
    OnAir(String),
    Reconnecting,
    Stopped,
    Lost(String),
}

/// What is on the deck, and the list it was started from.
pub struct NowPlaying {
    pub queue: Vec<Station>,
    pub index: usize,
    pub source: String,
    pub tune: u64,
    pub signal: Signal,
    pub title: Option<String>,
    /// Earlier titles this tune, newest first.
    pub history: Vec<String>,
}

impl NowPlaying {
    pub fn station(&self) -> &Station {
        &self.queue[self.index]
    }
}

/// Where oud lives, shown on the keys screen; opened by key or click.
pub struct Link {
    pub key: &'static str,
    pub label: &'static str,
    pub url: &'static str,
}

/// What the account screen offers once signed in: label and key.
pub const YOU_ROWS: [(&str, &str); 2] = [("library", "3"), ("sign out", "o")];

pub const LINKS: [Link; 2] = [
    Link { key: "o", label: "oud.so", url: "https://oud.so" },
    Link { key: "x", label: "made by @forwardset", url: "https://x.com/forwardset" },
];

/// What a clickable spot on screen leads to.
#[derive(Clone)]
pub enum Target {
    Url(String),
    Screen(Screen),
    /// One of [`SIGN_IN_WAYS`], by index.
    SignIn(usize),
    SignOut,
}

pub struct MapDrawing {
    station_id: String,
    size: (usize, usize),
    pub cells: Vec<Vec<map::Cell>>,
    pub marker: (usize, usize),
}

pub struct App {
    pub api: Api,
    pub player: Player,
    pub store: Store,
    tx: Sender<Msg>,
    pub quit: bool,
    pub screen: Screen,
    /// Where `?` returns to when pressed again on the about screen.
    previous_screen: Screen,

    pub daypart: &'static Daypart,
    pub something_tag: &'static str,
    pub explore_tab: ExploreTab,
    pub featured: Load<Vec<FeaturedList>>,
    pub signals: Load<Vec<Station>>,
    pub shuffling: bool,
    pub explore_sel: usize,
    pub open_list: Option<OpenList>,

    pub query: String,
    pub editing: bool,
    pub genre: Option<Genre>,
    pub country: Option<Country>,
    pub results: Load<Vec<Station>>,
    pub results_sel: usize,
    results_req: u64,
    pub picker: Option<Picker>,
    pub genres: Load<Vec<Genre>>,
    pub countries: Load<Vec<Country>>,

    pub library_tab: LibraryTab,
    pub library_sel: usize,
    pub lists_sel: usize,
    pub account: Account,
    pub sign_in: SignIn,
    /// Numbers sign-in attempts, so a late answer to an abandoned one is ignored.
    sign_in_attempt: u64,
    pub you_sel: usize,
    pub bookmarks: Load<Vec<Station>>,
    /// Ids of bookmarked stations, for marking rows.
    pub bookmarked: HashSet<String>,
    pub lists: Load<Vec<UserList>>,
    pub open_user_list: Option<OpenUserList>,

    pub now: Option<NowPlaying>,
    pub notice: Option<(String, Instant)>,
    pub spectrum: Spectrum,
    pub scope: Vec<f32>,
    pub visual: Visual,
    pub started: Instant,
    /// Clickable spots on screen this frame, with where they lead.
    pub links: Vec<(ratatui::layout::Rect, Target)>,
    pub about_sel: usize,
    /// The details panel above the player.
    pub show_details: bool,
    /// Loaded the first time details are opened; `None` until then or if it fails.
    map: Option<Map>,
    /// The last map drawn: station id and size, with its cells and marker.
    pub map_cache: Option<MapDrawing>,
}

impl App {
    pub fn new(api: Api, player: Player, store: Store, tx: Sender<Msg>) -> Self {
        let daypart = daypart::now();
        let mut app = Self {
            api,
            player,
            store,
            tx,
            quit: false,
            screen: Screen::Explore,
            previous_screen: Screen::Explore,
            daypart,
            something_tag: daypart.tags[fastrand::usize(..daypart.tags.len())],
            explore_tab: ExploreTab::Featured,
            featured: Load::Loading,
            signals: Load::Loading,
            shuffling: false,
            explore_sel: 0,
            open_list: None,
            query: String::new(),
            editing: false,
            genre: None,
            country: None,
            results: Load::Loading,
            results_sel: 0,
            results_req: 0,
            picker: None,
            genres: Load::Loading,
            countries: Load::Loading,
            library_tab: LibraryTab::Bookmarks,
            library_sel: 0,
            lists_sel: 0,
            account: Account::SignedOut,
            sign_in: SignIn::Choose,
            sign_in_attempt: 0,
            you_sel: 0,
            bookmarks: Load::Loading,
            bookmarked: HashSet::new(),
            lists: Load::Loading,
            open_user_list: None,
            now: None,
            notice: None,
            spectrum: Spectrum::new(2048),
            scope: Vec::new(),
            visual: Visual::new(),
            started: Instant::now(),
            links: Vec::new(),
            about_sel: 0,
            show_details: false,
            map: None,
            map_cache: None,
        };
        app.visual.set_mode(Pattern::from_name(&app.store.visual_mode));
        app.fetch(|api| Msg::Featured(api.featured()));
        app.fetch(|api| Msg::Signals(api.discover(SIGNAL_COUNT)));
        app.fetch(|api| Msg::Genres(api.genres()));
        app.fetch(|api| Msg::Countries(api.countries()));
        app.search();
        if let Some(token) = session::load() {
            app.api.set_token(Some(token));
            app.check_session();
        }
        app
    }

    fn fetch(&self, job: impl FnOnce(&Api) -> Msg + Send + 'static) {
        let (api, tx) = (self.api.clone(), self.tx.clone());
        thread::spawn(move || {
            let _ = tx.send(job(&api));
        });
    }

    pub fn notify(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now()));
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice
            .as_ref()
            .filter(|(_, at)| at.elapsed() < Duration::from_secs(3))
            .map(|(text, _)| text.as_str())
    }

    // ---- incoming -------------------------------------------------------

    pub fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Featured(r) => self.featured = r.into(),
            Msg::Signals(r) => {
                self.shuffling = false;
                match r {
                    Ok(v) if !v.is_empty() => self.signals = Load::Ready(v),
                    Ok(_) if self.signals.ready().is_some() => {}
                    other => self.signals = other.into(),
                }
            }
            Msg::List(slug, r) => {
                if let Some(open) = self.open_list.as_mut().filter(|o| o.list.slug == slug) {
                    open.stations = r.into();
                }
            }
            Msg::Stations(req, r) if req == self.results_req => {
                self.results = r.into();
                self.results_sel = 0;
            }
            Msg::Stations(..) => {}
            Msg::Genres(r) => self.genres = r.into(),
            Msg::Countries(r) => self.countries = r.into(),
            Msg::Something(Ok((tag, stations))) if !stations.is_empty() => {
                let index = fastrand::usize(..stations.len().min(12));
                self.play(stations, index, format!("{} · {tag}", self.daypart.source));
            }
            Msg::Something(Ok(_)) => self.notify("nothing on that frequency."),
            Msg::Something(Err(e)) => self.notify(e.to_string()),
            Msg::Session(r) => self.on_session(r),
            Msg::DeviceCode(attempt, r) if attempt == self.sign_in_attempt => self.on_device_code(r),
            Msg::SignedIn(attempt, r) if attempt == self.sign_in_attempt => self.on_signed_in(r),
            Msg::CodeSent(attempt, r) if attempt == self.sign_in_attempt => {
                if let SignIn::Email(step) = &mut self.sign_in {
                    step.busy = false;
                    match r {
                        Ok(()) => {
                            step.sent = true;
                            step.error = None;
                        }
                        Err(e) => step.error = Some(e.to_string()),
                    }
                }
            }
            Msg::DeviceCode(..) | Msg::SignedIn(..) | Msg::CodeSent(..) => {}
            Msg::Bookmarks(r) => {
                if let Ok(stations) = &r {
                    self.bookmarked = stations.iter().map(|s| s.id.clone()).collect();
                }
                self.bookmarks = r.into();
            }
            Msg::Lists(r) => self.lists = r.into(),
            Msg::UserList(id, r) => {
                if let Some(open) = self.open_user_list.as_mut().filter(|o| o.list.id == id) {
                    open.stations = r.into();
                }
            }
            Msg::BookmarkFailed(e) => {
                self.notify(format!("couldn't change that bookmark. {e}"));
                self.refresh_library();
            }
        }
    }

    pub fn on_audio(&mut self, event: Event) {
        let Some(now) = self.now.as_mut() else { return };
        match event {
            Event::Connecting { tune } if tune == now.tune => now.signal = Signal::Connecting,
            Event::OnAir { tune, format } if tune == now.tune => now.signal = Signal::OnAir(format),
            Event::Title { tune, title } if tune == now.tune => {
                if let Some(previous) = now.title.replace(title) {
                    now.history.insert(0, previous);
                    now.history.truncate(HISTORY);
                }
            }
            Event::Reconnecting { tune } if tune == now.tune => now.signal = Signal::Reconnecting,
            Event::Failed { tune, reason } if tune == now.tune => now.signal = Signal::Lost(reason),
            _ => {}
        }
    }

    // ---- playback -------------------------------------------------------

    pub fn play(&mut self, queue: Vec<Station>, index: usize, source: String) {
        let Some(station) = queue.get(index) else { return };
        let tune = self.player.play(station);
        self.visual.reshuffle();
        self.store.last = Some(station.clone());
        let _ = self.store.save();
        self.now = Some(NowPlaying {
            queue,
            index,
            source,
            tune,
            signal: Signal::Connecting,
            title: None,
            history: Vec::new(),
        });
    }

    fn step(&mut self, delta: isize) {
        let Some(now) = self.now.take() else { return };
        let len = now.queue.len() as isize;
        let index = (now.index as isize + delta).rem_euclid(len) as usize;
        self.play(now.queue, index, now.source);
    }

    fn toggle_playback(&mut self) {
        let Some(now) = self.now.as_mut() else {
            // Nothing tuned yet: bring back the last station, if any.
            if let Some(last) = self.store.last.clone() {
                self.play(vec![last], 0, "last time".into());
            }
            return;
        };
        if matches!(now.signal, Signal::Stopped | Signal::Lost(_)) {
            now.tune = self.player.play(now.station());
            now.signal = Signal::Connecting;
            now.title = None;
        } else {
            self.player.stop();
            now.signal = Signal::Stopped;
        }
    }

    fn nudge_volume(&mut self, delta: f32) {
        let deck = self.player.deck();
        deck.set_volume(deck.volume() + delta);
        self.store.volume = deck.volume();
    }

    // ---- what the current screen shows ----------------------------------

    /// Stations on screen right now, with the selected index and a name for
    /// the queue they would form.
    pub fn visible_stations(&self) -> Option<(&[Station], usize, String)> {
        match self.screen {
            Screen::Explore => match (&self.open_list, self.explore_tab) {
                (Some(open), _) => Some((open.stations.ready()?, open.sel, open.list.name.clone())),
                (None, ExploreTab::Signals) => {
                    Some((self.signals.ready()?, self.explore_sel.checked_sub(1)?, "small signals".into()))
                }
                (None, ExploreTab::Featured) => None,
            },
            Screen::Stations if self.picker.is_none() => {
                Some((self.results.ready()?, self.results_sel, self.results_source()))
            }
            Screen::Stations | Screen::About | Screen::You => None,
            Screen::Library => match (&self.open_user_list, self.library_tab) {
                (Some(open), _) => Some((open.stations.ready()?, open.sel, open.list.name.clone())),
                (None, LibraryTab::Bookmarks) => {
                    Some((self.bookmarks.ready()?, self.library_sel, "bookmarks".into()))
                }
                (None, LibraryTab::Lists) => None,
            },
        }
    }

    fn selected_station(&self) -> Option<&Station> {
        let (stations, sel, _) = self.visible_stations()?;
        stations.get(sel)
    }

    fn results_source(&self) -> String {
        let parts: Vec<String> = [
            Some(self.query.trim().to_string()).filter(|q| !q.is_empty()),
            self.genre.as_ref().map(|g| g.label().to_string()),
            self.country.as_ref().map(|c| c.name.clone()),
        ]
        .into_iter()
        .flatten()
        .collect();
        if parts.is_empty() { "stations".into() } else { parts.join(" · ") }
    }

    /// Rows in the current list, for moving the selection.
    fn row_count(&self) -> usize {
        match self.screen {
            Screen::Explore => match (&self.open_list, self.explore_tab) {
                (Some(open), _) => open.stations.ready().map_or(0, Vec::len),
                // Row 0 is "play something"; the list follows.
                (None, ExploreTab::Featured) => 1 + self.featured.ready().map_or(0, Vec::len),
                (None, ExploreTab::Signals) => 1 + self.signals.ready().map_or(0, Vec::len),
            },
            Screen::Stations => match &self.picker {
                Some(p) => self.picker_items(p).len(),
                None => self.results.ready().map_or(0, Vec::len),
            },
            Screen::Library => match (&self.open_user_list, self.library_tab) {
                (Some(open), _) => open.stations.ready().map_or(0, Vec::len),
                (None, LibraryTab::Bookmarks) => self.bookmarks.ready().map_or(0, Vec::len),
                (None, LibraryTab::Lists) => self.lists.ready().map_or(0, Vec::len),
            },
            Screen::About => LINKS.len(),
            Screen::You => match (&self.account, &self.sign_in) {
                (Account::SignedIn(_), _) => YOU_ROWS.len(),
                (Account::SignedOut, SignIn::Choose) => SIGN_IN_WAYS.len(),
                _ => 0,
            },
        }
    }

    fn selection_mut(&mut self) -> &mut usize {
        match self.screen {
            Screen::Explore => match &mut self.open_list {
                Some(open) => &mut open.sel,
                None => &mut self.explore_sel,
            },
            Screen::Stations => match &mut self.picker {
                Some(p) => &mut p.sel,
                None => &mut self.results_sel,
            },
            Screen::Library => match (&mut self.open_user_list, self.library_tab) {
                (Some(open), _) => &mut open.sel,
                (None, LibraryTab::Bookmarks) => &mut self.library_sel,
                (None, LibraryTab::Lists) => &mut self.lists_sel,
            },
            Screen::About => &mut self.about_sel,
            Screen::You => &mut self.you_sel,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let count = self.row_count();
        if count == 0 {
            return;
        }
        let sel = self.selection_mut();
        *sel = (*sel as isize + delta).clamp(0, count as isize - 1) as usize;
    }

    /// Picker rows: label, aside, and the index of the genre or country the
    /// row stands for. Until you type, row 0 (no index) clears the filter.
    pub fn picker_items(&self, picker: &Picker) -> Vec<(String, String, Option<usize>)> {
        let needle = picker.filter.to_lowercase();
        let keep = |s: &str| needle.is_empty() || s.to_lowercase().contains(&needle);
        let mut rows = Vec::new();
        match picker.kind {
            PickerKind::Genre => {
                if needle.is_empty() {
                    rows.push(("any genre".to_string(), String::new(), None));
                }
                for (i, g) in self.genres.ready().into_iter().flatten().enumerate() {
                    if keep(g.label()) {
                        rows.push((g.label().to_string(), g.count.to_string(), Some(i)));
                    }
                }
            }
            PickerKind::Country => {
                if needle.is_empty() {
                    rows.push(("any country".to_string(), String::new(), None));
                }
                for (i, c) in self.countries.ready().into_iter().flatten().enumerate() {
                    if keep(&c.name) {
                        rows.push((
                            c.name.clone(),
                            c.iso.clone().unwrap_or_default().to_lowercase(),
                            Some(i),
                        ));
                    }
                }
            }
        }
        rows
    }

    fn search(&mut self) {
        self.results_req += 1;
        self.results = Load::Loading;
        let req = self.results_req;
        let query = self.query.clone();
        let genre = self.genre.as_ref().map(|g| g.id.clone());
        let country = self.country.as_ref().map(|c| c.slug.clone());
        self.fetch(move |api| Msg::Stations(req, api.stations(&query, genre.as_deref(), country.as_deref())));
    }

    fn choose_picked(&mut self) {
        let Some(picker) = self.picker.take() else { return };
        let Some(&(_, _, index)) = self.picker_items(&picker).get(picker.sel) else { return };
        match picker.kind {
            PickerKind::Genre => {
                self.genre = index.and_then(|i| self.genres.ready()?.get(i).cloned());
            }
            PickerKind::Country => {
                self.country = index.and_then(|i| self.countries.ready()?.get(i).cloned());
            }
        }
        self.search();
    }

    // ---- keys -----------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.screen == Screen::About
            && let KeyCode::Char(c) = key.code
            && let Some(link) = LINKS.iter().find(|l| l.key == c.to_string())
        {
            return self.open_url(link.url);
        }
        if self.editing {
            return self.on_search_key(key);
        }
        if self.screen == Screen::You && self.on_you_key(key) {
            return;
        }
        if self.picker.is_some() && self.on_picker_key(key) {
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.toggle_about(),
            KeyCode::Char('1') => self.go(Screen::Explore),
            KeyCode::Char('2') => self.go(Screen::Stations),
            KeyCode::Char('3') => self.go(Screen::Library),
            KeyCode::Char('4') => self.go(Screen::About),
            KeyCode::Char('u') => self.toggle_you(),
            KeyCode::Tab => self.cycle_screen(1),
            KeyCode::BackTab => self.cycle_screen(-1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::Home => self.move_selection(-9999),
            KeyCode::End => self.move_selection(9999),
            KeyCode::Enter => self.activate(),
            KeyCode::Char(' ') => self.toggle_playback(),
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('[') => self.step(-1),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(']') => self.step(1),
            KeyCode::Char('+') | KeyCode::Char('=') => self.nudge_volume(VOLUME_STEP),
            KeyCode::Char('-') | KeyCode::Char('_') => self.nudge_volume(-VOLUME_STEP),
            KeyCode::Char('s') | KeyCode::Char('b') => self.toggle_bookmark(),
            KeyCode::Char('f') if self.screen == Screen::Explore => self.switch_explore_tab(),
            KeyCode::Char('r') if self.screen == Screen::Explore => self.reshuffle(),
            KeyCode::Char('/') => {
                self.screen = Screen::Stations;
                self.picker = None;
                self.editing = true;
            }
            KeyCode::Char('g') => self.open_picker(PickerKind::Genre),
            KeyCode::Char('c') if self.screen == Screen::Stations => self.open_picker(PickerKind::Country),
            KeyCode::Char('t') if self.screen == Screen::Library => self.switch_library_tab(),
            KeyCode::Char('i') => self.toggle_details(),
            KeyCode::Char('w') if self.show_details => {
                if let Some(home) = self.details_station().and_then(|s| s.homepage.clone()) {
                    self.open_url(&home);
                }
            }
            KeyCode::Char('v') => self.toggle_visual(),
            KeyCode::Char('e') => self.cycle_visual(1),
            KeyCode::Char('E') => self.cycle_visual(-1),
            KeyCode::Esc | KeyCode::Backspace => self.back(),
            _ => {}
        }
    }

    fn cycle_screen(&mut self, delta: isize) {
        let i = SCREENS.iter().position(|(s, _)| *s == self.screen).unwrap_or(0) as isize;
        self.go(SCREENS[(i + delta).rem_euclid(SCREENS.len() as isize) as usize].0);
    }

    fn go(&mut self, screen: Screen) {
        if screen != self.screen {
            self.previous_screen = self.screen;
            self.screen = screen;
        }
        // The library is the account's: read it fresh each visit.
        if screen == Screen::Library && matches!(self.account, Account::SignedIn(_)) {
            self.refresh_library();
        }
    }

    /// `u` opens the account, and from there goes back to where you were.
    fn toggle_you(&mut self) {
        if self.screen == Screen::You {
            self.screen = self.previous_screen;
        } else {
            self.you_sel = 0;
            self.go(Screen::You);
        }
    }

    /// `?` opens about, and from about goes back to where you were.
    fn toggle_about(&mut self) {
        if self.screen == Screen::About {
            self.screen = self.previous_screen;
        } else {
            self.go(Screen::About);
        }
    }

    fn on_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                self.editing = false;
                self.search();
            }
            KeyCode::Esc => self.editing = false,
            KeyCode::Backspace => {
                self.query.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => self.query.clear(),
            KeyCode::Char(c) => self.query.push(c),
            _ => {}
        }
    }

    /// Typing filters the picker. Returns true when the key was consumed.
    fn on_picker_key(&mut self, key: KeyEvent) -> bool {
        let picker = self.picker.as_mut().expect("picker is open");
        match key.code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Enter => self.choose_picked(),
            KeyCode::Backspace => {
                picker.filter.pop();
                picker.sel = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                picker.filter.push(c);
                picker.sel = 0;
            }
            _ => return false,
        }
        true
    }

    fn open_picker(&mut self, kind: PickerKind) {
        self.screen = Screen::Stations;
        self.editing = false;
        self.picker = Some(Picker { kind, filter: String::new(), sel: 0 });
    }

    fn activate(&mut self) {
        if self.screen == Screen::About {
            return self.open_url(LINKS[self.about_sel.min(LINKS.len() - 1)].url);
        }
        if self.screen == Screen::Library && self.open_user_list.is_none() {
            if !matches!(self.account, Account::SignedIn(_)) {
                return self.go(Screen::You);
            }
            if self.library_tab == LibraryTab::Lists {
                return self.open_user_list(self.lists_sel);
            }
        }
        if self.screen == Screen::Explore && self.open_list.is_none() {
            if self.explore_sel == 0 {
                return self.play_something();
            }
            if self.explore_tab == ExploreTab::Featured {
                return self.open_featured(self.explore_sel - 1);
            }
        }
        if self.screen == Screen::Stations && self.picker.is_some() {
            return self.choose_picked();
        }
        if let Some((stations, sel, source)) = self.visible_stations()
            && sel < stations.len()
        {
            let queue = stations.to_vec();
            self.play(queue, sel, source);
        }
    }

    fn play_something(&mut self) {
        let tag = self.something_tag.to_string();
        self.notify("tuning…");
        self.fetch(move |api| Msg::Something(api.stations_tagged(&tag, 40).map(|s| (tag, s))));
    }

    fn open_featured(&mut self, index: usize) {
        let Some(list) = self.featured.ready().and_then(|l| l.get(index)).cloned() else { return };
        let slug = list.slug.clone();
        self.open_list = Some(OpenList { list, stations: Load::Loading, sel: 0 });
        self.fetch(move |api| Msg::List(slug.clone(), api.featured_list(&slug)));
    }

    fn switch_explore_tab(&mut self) {
        self.open_list = None;
        self.explore_sel = 0;
        self.explore_tab = match self.explore_tab {
            ExploreTab::Featured => ExploreTab::Signals,
            ExploreTab::Signals => ExploreTab::Featured,
        };
    }

    fn reshuffle(&mut self) {
        if self.explore_tab == ExploreTab::Signals && !self.shuffling {
            self.shuffling = true;
            self.fetch(|api| Msg::Signals(api.discover(SIGNAL_COUNT)));
        }
    }

    fn switch_library_tab(&mut self) {
        self.open_user_list = None;
        self.library_tab = match self.library_tab {
            LibraryTab::Bookmarks => LibraryTab::Lists,
            LibraryTab::Lists => LibraryTab::Bookmarks,
        };
    }

    fn open_user_list(&mut self, index: usize) {
        let Some(list) = self.lists.ready().and_then(|l| l.get(index)).cloned() else { return };
        let id = list.id.clone();
        self.open_user_list = Some(OpenUserList { list, stations: Load::Loading, sel: 0 });
        self.fetch(move |api| Msg::UserList(id.clone(), api.list_stations(&id)));
    }

    /// A left click at a screen cell: follows a link if one is there.
    pub fn on_click(&mut self, column: u16, row: u16) {
        let hit =
            self.links.iter().find(|(area, _)| area.contains((column, row).into())).map(|(_, t)| t.clone());
        match hit {
            Some(Target::Url(url)) => self.open_url(&url),
            Some(Target::Screen(Screen::You)) if self.screen == Screen::You => {}
            Some(Target::Screen(Screen::You)) => self.toggle_you(),
            Some(Target::Screen(screen)) => self.go(screen),
            Some(Target::SignIn(way)) => match way {
                0 => self.start_browser_sign_in(),
                _ => self.start_email_sign_in(),
            },
            Some(Target::SignOut) => self.sign_out(),
            None => {}
        }
    }

    /// The station the details panel describes: what is on, or what was last on.
    pub fn details_station(&self) -> Option<&Station> {
        self.now.as_ref().map(NowPlaying::station).or(self.store.last.as_ref())
    }

    fn toggle_details(&mut self) {
        if self.details_station().is_none() {
            return self.notify("nothing to show yet. play a station first.");
        }
        self.show_details = !self.show_details;
        if self.show_details && self.map.is_none() {
            self.map = Map::load();
        }
    }

    /// The map for the details panel, drawn once per station and size.
    pub fn map_drawing(&mut self, cols: usize, rows: usize) -> Option<&MapDrawing> {
        let station = self.details_station()?.clone();
        let fresh =
            self.map_cache.as_ref().is_some_and(|d| d.station_id == station.id && d.size == (cols, rows));
        if !fresh {
            let map = self.map.as_mut()?;
            let place = match (station.geo_lat, station.geo_long) {
                (Some(lat), Some(lon)) => Some((lat, lon)),
                _ => station.country_code.as_deref().and_then(|iso| map.centre_of(iso)),
            };
            self.map_cache = place.map(|place| {
                let (cells, marker) = map.render(place, station.country_code.as_deref(), cols, rows);
                MapDrawing { station_id: station.id.clone(), size: (cols, rows), cells, marker }
            });
            // Nothing to draw for this station: remember that too, keyed the same way.
            if self.map_cache.is_none() {
                self.map_cache = Some(MapDrawing {
                    station_id: station.id,
                    size: (cols, rows),
                    cells: Vec::new(),
                    marker: (0, 0),
                });
            }
        }
        self.map_cache.as_ref().filter(|d| !d.cells.is_empty())
    }

    /// Open a web page in the default browser. Only http(s): addresses come
    /// from station data, and the system opener would follow any scheme.
    fn open_url(&mut self, url: &str) {
        let url = url.trim();
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return self.notify("that address can't be opened.");
        }
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let opened = std::process::Command::new(opener)
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        let shown = url.trim_start_matches("https://").trim_start_matches("http://");
        match opened {
            Ok(_) => self.notify(format!("opening {shown}…")),
            Err(_) => self.notify(format!("couldn't open a browser. {shown}")),
        }
    }

    fn toggle_visual(&mut self) {
        self.store.visual = !self.store.visual;
        let _ = self.store.save();
        self.notify(if self.store.visual { "visual on." } else { "visual off." });
    }

    fn cycle_visual(&mut self, step: isize) {
        if !self.store.visual {
            self.store.visual = true;
        } else {
            self.visual.cycle(step);
        }
        self.store.visual_mode = self.visual.mode().map(|p| p.name().to_string()).unwrap_or_default();
        let _ = self.store.save();
        self.notify(format!("visual · {}", self.visual.label()));
    }

    /// Bookmark the selected station, or the one playing; again to undo.
    /// Shown straight away, and put back if the server says no.
    fn toggle_bookmark(&mut self) {
        let target = self.selected_station().or(self.now.as_ref().map(NowPlaying::station)).cloned();
        let Some(station) = target else { return };
        if !matches!(self.account, Account::SignedIn(_)) {
            return self.notify("sign in to keep bookmarks. press u.");
        }
        let on = !self.bookmarked.contains(&station.id);
        if on {
            self.bookmarked.insert(station.id.clone());
        } else {
            self.bookmarked.remove(&station.id);
        }
        if let Load::Ready(list) = &mut self.bookmarks {
            list.retain(|s| s.id != station.id);
            if on {
                list.insert(0, station.clone());
            }
            if self.screen == Screen::Library && self.library_tab == LibraryTab::Bookmarks {
                self.library_sel = self.library_sel.min(list.len().saturating_sub(1));
            }
        }
        self.notify(if on { "bookmarked." } else { "bookmark removed." });
        let id = station.id;
        self.fetch(move |api| match api.set_bookmark(&id, on) {
            Ok(()) => Msg::Lists(api.lists()),
            Err(e) => Msg::BookmarkFailed(e.to_string()),
        });
    }

    // ---- the account ----------------------------------------------------

    fn check_session(&mut self) {
        self.account = Account::Checking;
        self.fetch(|api| Msg::Session(api.session()));
    }

    fn on_session(&mut self, r: anyhow::Result<Option<User>>) {
        match r {
            Ok(Some(user)) => {
                // Arriving from a sign-in rather than from a token kept since last time.
                if !matches!(self.sign_in, SignIn::Choose) {
                    self.notify(format!("signed in as {}.", user.email));
                }
                self.account = Account::SignedIn(user);
                self.sign_in = SignIn::Choose;
                self.you_sel = 0;
                self.refresh_library();
            }
            // The token is no longer any good: forget it.
            Ok(None) => {
                session::clear();
                self.api.set_token(None);
                self.account = Account::SignedOut;
            }
            Err(e) => self.account = Account::Unreachable(e.to_string()),
        }
    }

    fn refresh_library(&mut self) {
        self.fetch(|api| Msg::Bookmarks(api.bookmarks()));
        self.fetch(|api| Msg::Lists(api.lists()));
    }

    /// A fresh sign-in attempt; anything still answering the old one is ignored.
    fn next_attempt(&mut self) -> u64 {
        if let SignIn::Browser { cancel, .. } = &self.sign_in {
            cancel.store(true, Ordering::Relaxed);
        }
        self.sign_in_attempt += 1;
        self.sign_in_attempt
    }

    fn start_browser_sign_in(&mut self) {
        let attempt = self.next_attempt();
        let cancel = Arc::new(AtomicBool::new(false));
        self.sign_in = SignIn::Browser { code: None, failed: None, cancel: cancel.clone() };
        let tx = self.tx.clone();
        let api = self.api.clone();
        thread::spawn(move || {
            let code = api.device_code();
            let started = code.as_ref().ok().map(|c| (c.device_code.clone(), c.interval, c.expires_in));
            let _ = tx.send(Msg::DeviceCode(attempt, code));
            let Some((device_code, interval, expires_in)) = started else { return };
            let result = wait_for_approval(&api, &device_code, interval, expires_in, &cancel);
            if !cancel.load(Ordering::Relaxed) {
                let _ = tx.send(Msg::SignedIn(attempt, result));
            }
        });
    }

    fn on_device_code(&mut self, r: anyhow::Result<DeviceCode>) {
        let SignIn::Browser { code, failed, .. } = &mut self.sign_in else { return };
        match r {
            Ok(c) => {
                let url = c.verification_uri_complete.clone();
                *code = Some(c);
                self.open_url(&url);
            }
            Err(e) => *failed = Some(e.to_string()),
        }
    }

    fn on_signed_in(&mut self, r: anyhow::Result<String>) {
        match r {
            Ok(token) => {
                if let Err(e) = session::save(&token) {
                    self.notify(format!("signed in, but couldn't keep the session. {e}"));
                }
                self.api.set_token(Some(token));
                self.check_session();
            }
            Err(e) => match &mut self.sign_in {
                SignIn::Browser { failed, .. } => *failed = Some(e.to_string()),
                SignIn::Email(step) => {
                    step.busy = false;
                    step.error = Some(e.to_string());
                }
                SignIn::Choose => {}
            },
        }
    }

    fn sign_out(&mut self) {
        if let Some(token) = session::load() {
            let api = self.api.clone();
            thread::spawn(move || api.sign_out(&token));
        }
        session::clear();
        self.api.set_token(None);
        self.account = Account::SignedOut;
        self.sign_in = SignIn::Choose;
        self.you_sel = 0;
        self.bookmarks = Load::Loading;
        self.bookmarked.clear();
        self.lists = Load::Loading;
        self.open_user_list = None;
        self.notify("signed out.");
    }

    /// Keys on the account screen. Returns true when the key was used.
    fn on_you_key(&mut self, key: KeyEvent) -> bool {
        match (&self.account, &self.sign_in) {
            (Account::SignedIn(_), _) => match key.code {
                KeyCode::Char('o') => self.sign_out(),
                KeyCode::Enter => match YOU_ROWS.get(self.you_sel).map(|r| r.1) {
                    Some("3") => self.go(Screen::Library),
                    Some("o") => self.sign_out(),
                    _ => {}
                },
                _ => return false,
            },
            (Account::Unreachable(_), _) => match key.code {
                KeyCode::Enter => self.check_session(),
                _ => return false,
            },
            (Account::Checking, _) => return false,
            (Account::SignedOut, SignIn::Choose) => match key.code {
                KeyCode::Char('b') => self.start_browser_sign_in(),
                KeyCode::Char('m') => self.start_email_sign_in(),
                KeyCode::Enter if self.you_sel == 0 => self.start_browser_sign_in(),
                KeyCode::Enter => self.start_email_sign_in(),
                _ => return false,
            },
            (Account::SignedOut, SignIn::Browser { code, .. }) => match key.code {
                KeyCode::Enter => {
                    if let Some(url) = code.as_ref().map(|c| c.verification_uri_complete.clone()) {
                        self.open_url(&url);
                    }
                }
                KeyCode::Esc | KeyCode::Backspace => {
                    self.next_attempt();
                    self.sign_in = SignIn::Choose;
                }
                _ => return false,
            },
            (Account::SignedOut, SignIn::Email(step)) => {
                let sent = step.sent;
                return self.on_email_key(key, sent);
            }
        }
        true
    }

    fn start_email_sign_in(&mut self) {
        self.next_attempt();
        self.sign_in = SignIn::Email(EmailSignIn::default());
    }

    /// Typing into the email form. Every printable key is text here.
    fn on_email_key(&mut self, key: KeyEvent, sent: bool) -> bool {
        let SignIn::Email(step) = &mut self.sign_in else { return false };
        let field = if sent { &mut step.code } else { &mut step.email };
        match key.code {
            KeyCode::Esc if sent => {
                step.sent = false;
                step.code.clear();
                step.error = None;
            }
            KeyCode::Esc => {
                self.next_attempt();
                self.sign_in = SignIn::Choose;
            }
            KeyCode::Backspace => {
                field.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => field.clear(),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                // A code is digits only; anything else is a slip of the finger.
                if !sent || (c.is_ascii_digit() && field.len() < 6) {
                    field.push(c);
                }
            }
            KeyCode::Enter if !step.busy => self.submit_email(),
            _ => {}
        }
        true
    }

    fn submit_email(&mut self) {
        let attempt = self.sign_in_attempt;
        let SignIn::Email(step) = &mut self.sign_in else { return };
        let email = step.email.trim().to_lowercase();
        if !step.sent {
            if !email.contains('@') {
                step.error = Some("that doesn't look like an email.".into());
                return;
            }
            step.busy = true;
            step.error = None;
            self.fetch(move |api| Msg::CodeSent(attempt, api.send_code(&email)));
        } else {
            let code = step.code.clone();
            if code.len() < 6 {
                step.error = Some("the code has six digits.".into());
                return;
            }
            step.busy = true;
            step.error = None;
            self.fetch(move |api| Msg::SignedIn(attempt, api.verify_code(&email, &code)));
        }
    }

    fn back(&mut self) {
        match self.screen {
            Screen::About | Screen::You => self.screen = self.previous_screen,
            Screen::Library if self.open_user_list.is_some() => self.open_user_list = None,
            Screen::Explore if self.open_list.is_some() => self.open_list = None,
            Screen::Stations if !self.query.is_empty() || self.genre.is_some() || self.country.is_some() => {
                self.query.clear();
                self.genre = None;
                self.country = None;
                self.search();
            }
            _ => {}
        }
    }
}

/// Ask, every `interval` seconds, whether the code was approved, until it is,
/// it is turned down, it runs out, or `cancel` is set.
fn wait_for_approval(
    api: &Api,
    device_code: &str,
    interval: u64,
    expires_in: u64,
    cancel: &AtomicBool,
) -> anyhow::Result<String> {
    let deadline = Instant::now() + Duration::from_secs(expires_in);
    let mut interval = Duration::from_secs(interval.max(1));
    loop {
        // Sleep in small steps so cancelling is felt at once.
        let wake = Instant::now() + interval;
        while Instant::now() < wake {
            if cancel.load(Ordering::Relaxed) {
                anyhow::bail!("cancelled.");
            }
            thread::sleep(Duration::from_millis(100));
        }
        if Instant::now() > deadline {
            anyhow::bail!("the code ran out. try again.");
        }
        match api.device_poll(device_code) {
            Ok(DevicePoll::Approved(token)) => return Ok(token),
            Ok(DevicePoll::Waiting) => {}
            Ok(DevicePoll::SlowDown) => interval += Duration::from_secs(5),
            Ok(DevicePoll::Denied) => anyhow::bail!("turned down in the browser."),
            Ok(DevicePoll::Expired) => anyhow::bail!("the code ran out. try again."),
            // A dropped connection is not the end: ask again next time round.
            Err(_) => {}
        }
    }
}
