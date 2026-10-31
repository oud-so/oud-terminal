//! The full-screen interface.

mod app;
mod daypart;
mod view;

use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event as TermEvent, KeyEventKind, MouseButton,
    MouseEventKind,
};
use crossterm::execute;

use crate::api::Api;
use crate::audio::Player;
use crate::store::Store;
use app::App;

/// Redraw often enough for the level meter to move smoothly.
const FRAME: Duration = Duration::from_millis(40);

pub fn run(api: Api) -> Result<()> {
    let store = Store::load();
    let (audio_tx, audio_rx) = mpsc::channel();
    // Open the sound card before taking over the screen, so a missing device
    // is reported as a plain error rather than inside a half-drawn interface.
    let player = Player::new(api.clone(), store.volume, audio_tx)?;

    let (tx, rx) = mpsc::channel();
    let mut app = App::new(api, player, store, tx);
    let mut terminal = ratatui::init();

    // The mouse is captured so links can be clicked. Most terminals still
    // select text when a modifier (often ⌥) is held while dragging.
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let result = (|| -> Result<()> {
        while !app.quit {
            terminal.draw(|f| view::draw(f, &mut app))?;
            if event::poll(FRAME)? {
                match event::read()? {
                    TermEvent::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key),
                    TermEvent::Mouse(m) if m.kind == MouseEventKind::Down(MouseButton::Left) => {
                        app.on_click(m.column, m.row)
                    }
                    _ => {}
                }
            }
            while let Ok(msg) = rx.try_recv() {
                app.on_msg(msg);
            }
            while let Ok(event) = audio_rx.try_recv() {
                app.on_audio(event);
            }
        }
        Ok(())
    })();

    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    app.player.stop();
    let _ = app.store.save();
    result
}
