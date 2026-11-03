//! Door control through the genuine myQ app on the owner's Android phone, over adb.
//! myQ's token endpoint serves only its attested app (docs/api.md), so the app does
//! the talking: this reads its screen and taps its door button.
//!
//! A tap moves a real door, so one is sent only when the screen read just before it
//! shows exactly one door, at rest, in the state opposite the one asked for. Nothing
//! retries a tap. Another app's screen is never read beyond its package name.

use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pk_cli_core::CliError;

use crate::door::{self, Door, DoorChange, DoorCommand, Wait};

const PACKAGE: &str = "com.chamberlain.android.liftmaster.myq";
/// Names the adb program, overriding the search of the usual SDK locations.
pub const ENV_ADB: &str = "MYQ_ADB";

const APP_START_TRIES: u32 = 10;
const APP_START_WAIT: Duration = Duration::from_secs(1);
const ADB_TIMEOUT: Duration = Duration::from_secs(30);

/// What the door logic needs from the phone, so the tests never touch a real one.
pub trait Phone {
    /// The foreground window's UI hierarchy as XML.
    fn screen_xml(&self) -> Result<String, CliError>;
    fn open_app(&self) -> Result<(), CliError>;
    fn tap(&self, x: u32, y: u32) -> Result<(), CliError>;
}

pub struct Adb {
    program: OsString,
}

impl Adb {
    pub fn locate() -> Self {
        Self {
            program: std::env::var_os(ENV_ADB)
                .or_else(sdk_adb)
                .unwrap_or_else(|| "adb".into()),
        }
    }

    fn run(&self, args: &[&str]) -> Result<String, CliError> {
        let mut child = Command::new(&self.program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => CliError::Upstream(format!(
                    "`adb` was not found. Install Android's platform-tools, or name it with \
                     {ENV_ADB}"
                )),
                _ => CliError::Upstream(format!("adb did not start: {err}")),
            })?;
        // A screen is larger than a pipe's buffer, so both pipes drain while adb runs.
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let deadline = Instant::now() + ADB_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(CliError::Upstream(format!(
                        "adb did not answer within {} seconds",
                        ADB_TIMEOUT.as_secs()
                    )));
                }
                Err(err) => return Err(CliError::Upstream(format!("adb: {err}"))),
            }
        };
        let out = stdout.join().unwrap_or_default();
        let err = stderr.join().unwrap_or_default();
        if status.success() {
            Ok(out)
        } else {
            Err(adb_failure(&err))
        }
    }
}

impl Phone for Adb {
    fn screen_xml(&self) -> Result<String, CliError> {
        self.run(&["exec-out", "uiautomator", "dump", "/dev/tty"])
    }

    /// `am start` rather than `monkey`, whose other events are random taps.
    fn open_app(&self) -> Result<(), CliError> {
        let resolved = self.run(&[
            "shell",
            "cmd",
            "package",
            "resolve-activity",
            "--brief",
            "-c",
            "android.intent.category.LAUNCHER",
            PACKAGE,
        ])?;
        let component = resolved
            .lines()
            .map(str::trim)
            .rfind(|line| line.starts_with(PACKAGE) && line.contains('/'))
            .ok_or_else(|| CliError::Upstream("the myQ app is not on this phone".into()))?;
        self.run(&["shell", "am", "start", "-n", component])?;
        Ok(())
    }

    fn tap(&self, x: u32, y: u32) -> Result<(), CliError> {
        let (x, y) = (x.to_string(), y.to_string());
        self.run(&["shell", "input", "tap", &x, &y])?;
        Ok(())
    }
}

fn sdk_adb() -> Option<OsString> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let roots = [
        std::env::var_os("ANDROID_HOME").map(PathBuf::from),
        std::env::var_os("ANDROID_SDK_ROOT").map(PathBuf::from),
        home.as_ref().map(|h| h.join("Library/Android/sdk")),
        home.as_ref().map(|h| h.join("Android/Sdk")),
    ];
    roots
        .into_iter()
        .flatten()
        .map(|root| root.join("platform-tools/adb"))
        .find(|path| path.is_file())
        .map(PathBuf::into_os_string)
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

fn adb_failure(stderr: &str) -> CliError {
    let text = stderr.to_ascii_lowercase();
    if text.contains("unauthorized") {
        CliError::Auth(
            "the phone has not allowed this computer: unlock it and accept the USB debugging \
             prompt"
                .into(),
        )
    } else if text.contains("more than one") {
        CliError::Usage(
            "more than one Android device is connected: name one with ANDROID_SERIAL".into(),
        )
    } else if text.contains("no devices")
        || text.contains("device offline")
        || (text.contains("device '") && text.contains("not found"))
    {
        CliError::Upstream(
            "no phone is connected: plug it in over USB or use wireless debugging, and allow \
             debugging on the phone"
                .into(),
        )
    } else {
        let reason = stderr.lines().next().unwrap_or("no reason given");
        CliError::Upstream(format!("adb failed: {reason}"))
    }
}

/// One door as the app's home screen shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Card {
    door: Door,
    button: Option<(u32, u32)>,
}

#[derive(Debug, PartialEq, Eq)]
enum Screen {
    Doors(Vec<Card>),
    AppWithoutDoors,
    OtherApp,
    /// `uiautomator` found no idle moment, which an animation such as a moving door
    /// can cause, or printed no hierarchy.
    Unreadable,
}

fn parse_screen(xml: &str) -> Screen {
    let nodes: Vec<Vec<(String, String)>> = node_tags(xml).iter().map(|t| attributes(t)).collect();
    if nodes.is_empty() {
        return Screen::Unreadable;
    }
    if !nodes.iter().all(|n| attr(n, "package") == Some(PACKAGE)) {
        return Screen::OtherApp;
    }
    let mut cards: Vec<Card> = Vec::new();
    for node in &nodes {
        let id = attr(node, "resource-id").unwrap_or_default();
        match id.rsplit('/').next().unwrap_or_default() {
            "device_name" => cards.push(Card {
                door: Door {
                    name: attr(node, "text").unwrap_or_default().to_string(),
                    serial: None,
                    state: "unknown".into(),
                    online: None,
                    last_update: None,
                    status: None,
                },
                button: None,
            }),
            "device_status" => {
                if let Some(card) = cards.last_mut() {
                    let status = attr(node, "text").unwrap_or_default();
                    card.door.state = state_of(status).into();
                    card.door.online = first_word(status).eq("offline").then_some(false);
                    card.door.status = Some(status.to_string());
                }
            }
            "device_state_icon" => {
                let usable = attr(node, "clickable") == Some("true")
                    && attr(node, "enabled") == Some("true");
                if let (true, Some(card)) = (usable, cards.last_mut()) {
                    card.button = attr(node, "bounds").and_then(center);
                }
            }
            _ => {}
        }
    }
    cards.retain(|card| !card.door.name.is_empty());
    if cards.is_empty() {
        Screen::AppWithoutDoors
    } else {
        Screen::Doors(cards)
    }
}

fn first_word(status: &str) -> String {
    status
        .split(|c: char| !c.is_alphabetic())
        .next()
        .unwrap_or_default()
        .to_lowercase()
}

/// The app's "Closed for 14 minutes" as a `door_state`. Anything unrecognised is
/// `unknown`, which never earns a tap.
fn state_of(status: &str) -> &'static str {
    match first_word(status).as_str() {
        "closed" => "closed",
        "closing" => "closing",
        "open" | "opened" => "open",
        "opening" => "opening",
        _ => "unknown",
    }
}

/// `[712,883][975,1146]` as its center.
fn center(bounds: &str) -> Option<(u32, u32)> {
    let numbers: Vec<u32> = bounds
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    match numbers.as_slice() {
        [left, top, right, bottom] if left < right && top < bottom => {
            Some(((left + right) / 2, (top + bottom) / 2))
        }
        _ => None,
    }
}

/// Each `<node …>` opening tag. A value may hold `>`, so quotes are tracked.
fn node_tags(xml: &str) -> Vec<&str> {
    let mut tags = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<node ") {
        let tag = &rest[start..];
        let mut quoted = false;
        let Some(end) = tag.char_indices().find_map(|(i, c)| {
            if c == '"' {
                quoted = !quoted;
            }
            (c == '>' && !quoted).then_some(i)
        }) else {
            break;
        };
        tags.push(&tag[..end]);
        rest = &tag[end..];
    }
    tags
}

fn attributes(tag: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut rest = tag.strip_prefix("<node").unwrap_or(tag);
    while let Some(eq) = rest.find("=\"") {
        let name = rest[..eq].trim();
        let after = &rest[eq + 2..];
        let Some(close) = after.find('"') else { break };
        found.push((
            name.to_string(),
            pk_cli_scrape::decode_entities(&after[..close]),
        ));
        rest = &after[close + 1..];
    }
    found
}

fn attr<'a>(node: &'a [(String, String)], name: &str) -> Option<&'a str> {
    node.iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// The doors on the app's home screen, bringing the app forward when it is not there.
fn door_cards<P: Phone>(phone: &P, sleep: &dyn Fn(Duration)) -> Result<Vec<Card>, CliError> {
    let mut screen = parse_screen(&phone.screen_xml()?);
    if !matches!(screen, Screen::Doors(_)) {
        phone.open_app()?;
        for _ in 0..APP_START_TRIES {
            sleep(APP_START_WAIT);
            screen = parse_screen(&phone.screen_xml()?);
            if matches!(screen, Screen::Doors(_)) {
                break;
            }
        }
    }
    match screen {
        Screen::Doors(cards) => Ok(cards),
        Screen::AppWithoutDoors => Err(CliError::Upstream(
            "myQ is open on the phone but no door is on its screen. Go to its Home tab, and \
             sign in if it asks"
                .into(),
        )),
        Screen::OtherApp => Err(CliError::Upstream(
            "the myQ app did not come to the phone's screen. Unlock the phone and try again".into(),
        )),
        Screen::Unreadable => Err(CliError::Upstream(
            "the phone's screen could not be read. Try again".into(),
        )),
    }
}

/// One read that changes nothing on the phone. `None` when no door is on its screen.
pub fn doors_on_screen<P: Phone>(phone: &P) -> Result<Option<Vec<Door>>, CliError> {
    Ok(match parse_screen(&phone.screen_xml()?) {
        Screen::Doors(cards) => Some(cards.into_iter().map(|card| card.door).collect()),
        _ => None,
    })
}

/// The doors, bringing the app to the phone's screen if they are not on it.
pub fn doors<P: Phone>(phone: &P, sleep: &dyn Fn(Duration)) -> Result<Vec<Door>, CliError> {
    Ok(door_cards(phone, sleep)?
        .into_iter()
        .map(|card| card.door)
        .collect())
}

/// Tap a door's button if that is what it takes, then read back until the door shows
/// the requested state. A read that fails while waiting is not an error yet, because
/// the door is moving either way, and nothing here taps a second time.
pub fn set_door<P: Phone>(
    phone: &P,
    query: &str,
    command: DoorCommand,
    wait: Wait,
    sleep: &dyn Fn(Duration),
) -> Result<DoorChange, CliError> {
    let cards = door_cards(phone, sleep)?;
    let doors: Vec<Door> = cards.iter().map(|card| card.door.clone()).collect();
    let door = door::find(&doors, query)?.clone();
    if door.state == command.target() {
        return Ok(DoorChange {
            door,
            changed: false,
            confirmed: true,
        });
    }
    let sent = door.state != command.moving();
    if sent {
        let at_rest_opposite = match command {
            DoorCommand::Open => "closed",
            DoorCommand::Close => "open",
        };
        if door.state != at_rest_opposite {
            return Err(CliError::Upstream(format!(
                "`{}` reads `{}`, so nothing was tapped. A tap now could stop or reverse it, so \
                 check the door and try again once it has stopped",
                door.name,
                door.status.as_deref().unwrap_or(&door.state)
            )));
        }
        let [card] = cards.as_slice() else {
            return Err(CliError::Upstream(format!(
                "{} doors are on the phone's screen, and telling their buttons apart has not \
                 been tested, so nothing was tapped",
                cards.len()
            )));
        };
        let Some((x, y)) = card.button else {
            return Err(CliError::Upstream(format!(
                "`{}` has no door button on the phone's screen, so nothing was tapped",
                door.name
            )));
        };
        phone
            .tap(x, y)
            .map_err(|err| tap_unknown(&door.name, err))?;
    }

    let name = door.name.clone();
    let mut latest = door;
    let reached = |door: Door, confirmed: bool| DoorChange {
        door,
        changed: sent,
        confirmed,
    };
    if wait.timeout.is_zero() {
        let read = read_door(phone, &name).unwrap_or(Door {
            state: "unknown".into(),
            status: None,
            ..latest
        });
        let confirmed = read.state == command.target();
        return Ok(reached(read, confirmed));
    }
    // By the clock, not a count of polls: one read can take as long as `ADB_TIMEOUT`.
    let started = Instant::now();
    while started.elapsed() < wait.timeout {
        sleep(wait.interval);
        if let Some(read) = read_door(phone, &name) {
            latest = read;
            if latest.state == command.target() {
                return Ok(reached(latest, true));
            }
        }
    }
    let opening = if sent {
        "The door button was tapped, but"
    } else {
        "Nothing was tapped because the door was already moving, and"
    };
    Err(CliError::Upstream(format!(
        "{opening} `{name}` last read `{}` after {} seconds. Check the door before sending it \
         again",
        latest.status.as_deref().unwrap_or(&latest.state),
        started.elapsed().as_secs()
    )))
}

/// A failed `input tap` may still have reached the phone, so this must not read as
/// nothing having happened.
fn tap_unknown(name: &str, err: CliError) -> CliError {
    let text = format!(
        "The tap on `{name}`'s door button may have reached the phone, so the door may be \
         moving. Check it before trying again. {err}"
    );
    match err {
        CliError::Auth(_) => CliError::Auth(text),
        _ => CliError::Upstream(text),
    }
}

fn read_door<P: Phone>(phone: &P, name: &str) -> Option<Door> {
    match parse_screen(&phone.screen_xml().ok()?) {
        Screen::Doors(cards) => cards
            .into_iter()
            .map(|card| card.door)
            .find(|door| door.name == name),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    const HOME: &str = include_str!("../tests/fixtures/phone_home.xml");
    const FAST: Wait = Wait {
        timeout: Duration::from_millis(30),
        interval: Duration::from_millis(10),
    };

    /// Plays its screens in order and repeats the last, recording what it was told.
    struct Fake {
        screens: RefCell<VecDeque<String>>,
        taps: RefCell<Vec<(u32, u32)>>,
        opened: Cell<u32>,
        reads: Cell<u32>,
        read_delay: Duration,
        tap_fails: bool,
    }

    impl Fake {
        fn new(screens: &[String]) -> Self {
            Self {
                screens: RefCell::new(screens.iter().cloned().collect()),
                taps: RefCell::default(),
                opened: Cell::new(0),
                reads: Cell::new(0),
                read_delay: Duration::ZERO,
                tap_fails: false,
            }
        }
    }

    impl Phone for Fake {
        fn screen_xml(&self) -> Result<String, CliError> {
            self.reads.set(self.reads.get() + 1);
            std::thread::sleep(self.read_delay);
            let mut screens = self.screens.borrow_mut();
            let next = if screens.len() > 1 {
                screens.pop_front()
            } else {
                screens.front().cloned()
            };
            Ok(next.expect("a screen"))
        }

        fn open_app(&self) -> Result<(), CliError> {
            self.opened.set(self.opened.get() + 1);
            Ok(())
        }

        fn tap(&self, x: u32, y: u32) -> Result<(), CliError> {
            self.taps.borrow_mut().push((x, y));
            if self.tap_fails {
                return Err(CliError::Upstream("adb did not answer".into()));
            }
            Ok(())
        }
    }

    fn reading(status: &str) -> String {
        HOME.replace("Closed for 14 minutes", status)
    }

    fn other_app() -> String {
        "<hierarchy rotation=\"0\"><node text=\"dummy private text\" resource-id=\"\" \
         package=\"com.example.other\" bounds=\"[0,0][1,1]\"></node></hierarchy>"
            .to_string()
    }

    fn two_doors() -> String {
        let card = HOME.find("<node index=\"1\"").unwrap();
        let end = HOME.rfind("</hierarchy>").unwrap();
        let body = &HOME[card..end];
        HOME.replace("</hierarchy>", &format!("{body}</hierarchy>"))
    }

    fn set(fake: &Fake, command: DoorCommand) -> Result<DoorChange, CliError> {
        set_door(fake, "garage", command, FAST, &|_| {})
    }

    #[test]
    fn the_home_screen_reads_as_one_door_with_its_button() {
        let Screen::Doors(cards) = parse_screen(HOME) else {
            panic!("expected a door");
        };
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].door.name, "Dummy Garage");
        assert_eq!(cards[0].door.state, "closed");
        assert_eq!(
            cards[0].door.status.as_deref(),
            Some("Closed for 14 minutes")
        );
        assert_eq!(cards[0].button, Some((843, 1014)));
    }

    #[test]
    fn the_apps_wording_maps_to_a_door_state() {
        for (status, state) in [
            ("Closed for 14 minutes", "closed"),
            ("Closed", "closed"),
            ("Closing", "closing"),
            ("Closing…", "closing"),
            ("Open for 2 hours", "open"),
            ("Open", "open"),
            ("Opening", "opening"),
            ("Offline", "unknown"),
            ("Stopped", "unknown"),
            ("", "unknown"),
        ] {
            assert_eq!(state_of(status), state, "{status}");
        }
    }

    #[test]
    fn an_offline_door_says_so() {
        let Screen::Doors(cards) = parse_screen(&reading("Offline")) else {
            panic!("expected a door");
        };
        assert_eq!(cards[0].door.online, Some(false));
    }

    #[test]
    fn a_value_may_hold_a_closing_bracket_or_an_entity() {
        let page = HOME.replace("Dummy Garage", "Dummy &amp; a > b");
        let Screen::Doors(cards) = parse_screen(&page) else {
            panic!("expected a door");
        };
        assert_eq!(cards[0].door.name, "Dummy & a > b");
    }

    #[test]
    fn a_package_prefixed_id_is_the_same_id() {
        let page = HOME
            .replace(
                "resource-id=\"device_name\"",
                "resource-id=\"com.example:id/device_name\"",
            )
            .replace(
                "resource-id=\"device_status\"",
                "resource-id=\"com.example:id/device_status\"",
            );
        let Screen::Doors(cards) = parse_screen(&page) else {
            panic!("expected a door");
        };
        assert_eq!(cards[0].door.state, "closed");
    }

    #[test]
    fn a_failed_dump_is_unreadable() {
        assert_eq!(
            parse_screen("ERROR: could not get idle state."),
            Screen::Unreadable
        );
    }

    #[test]
    fn reading_never_taps() {
        let fake = Fake::new(&[HOME.to_string()]);
        let doors = doors(&fake, &|_| {}).unwrap();
        assert_eq!(doors[0].state, "closed");
        assert!(fake.taps.borrow().is_empty());
        assert_eq!(fake.opened.get(), 0, "the app was already in front");
    }

    #[test]
    fn reading_the_screen_alone_changes_nothing_on_the_phone() {
        let fake = Fake::new(&[other_app()]);
        assert_eq!(doors_on_screen(&fake).unwrap(), None);
        assert_eq!(fake.opened.get(), 0);

        let fake = Fake::new(&[HOME.to_string()]);
        assert_eq!(doors_on_screen(&fake).unwrap().unwrap().len(), 1);
        assert_eq!(fake.opened.get(), 0);
        assert!(fake.taps.borrow().is_empty());
    }

    #[test]
    fn reading_brings_the_app_forward_when_another_app_is_in_front() {
        let fake = Fake::new(&[other_app(), other_app(), HOME.to_string()]);
        assert_eq!(doors(&fake, &|_| {}).unwrap().len(), 1);
        assert_eq!(fake.opened.get(), 1);
        assert!(fake.taps.borrow().is_empty());
    }

    #[test]
    fn a_locked_phone_says_to_unlock_and_shows_nothing_of_the_other_screen() {
        let fake = Fake::new(&[other_app()]);
        let err = doors(&fake, &|_| {}).unwrap_err();
        assert_eq!(err.exit_code(), 5);
        assert!(err.to_string().contains("Unlock"), "{err}");
        assert!(!err.to_string().contains("dummy private"), "{err}");
    }

    #[test]
    fn the_app_on_another_tab_says_where_to_go() {
        let page = HOME.replace("resource-id=\"device_name\"", "resource-id=\"other\"");
        let err = doors(&Fake::new(&[page]), &|_| {}).unwrap_err();
        assert!(err.to_string().contains("Home tab"), "{err}");
    }

    #[test]
    fn opening_a_closed_door_taps_its_button_once_and_waits_for_open() {
        let fake = Fake::new(&[HOME.to_string(), reading("Opening"), reading("Open")]);
        let change = set(&fake, DoorCommand::Open).unwrap();
        assert!(change.changed && change.confirmed);
        assert_eq!(change.door.state, "open");
        assert_eq!(*fake.taps.borrow(), vec![(843, 1014)]);
    }

    #[test]
    fn closing_an_open_door_taps_its_button() {
        let fake = Fake::new(&[reading("Open for 2 hours"), reading("Closed")]);
        let change = set(&fake, DoorCommand::Close).unwrap();
        assert!(change.changed && change.confirmed);
        assert_eq!(fake.taps.borrow().len(), 1);
    }

    #[test]
    fn a_door_already_there_gets_no_tap() {
        let fake = Fake::new(&[HOME.to_string()]);
        let change = set(&fake, DoorCommand::Close).unwrap();
        assert!(!change.changed && change.confirmed);
        assert!(fake.taps.borrow().is_empty());
    }

    #[test]
    fn a_door_already_on_its_way_gets_no_tap() {
        let fake = Fake::new(&[reading("Opening"), reading("Open")]);
        let change = set(&fake, DoorCommand::Open).unwrap();
        assert!(!change.changed && change.confirmed);
        assert!(fake.taps.borrow().is_empty());
    }

    #[test]
    fn a_door_moving_the_other_way_or_in_an_unknown_state_gets_no_tap() {
        for (status, command) in [
            ("Closing", DoorCommand::Open),
            ("Opening", DoorCommand::Close),
            ("Offline", DoorCommand::Open),
            ("Stopped", DoorCommand::Close),
            ("", DoorCommand::Open),
        ] {
            let fake = Fake::new(&[reading(status)]);
            let err = set(&fake, command).unwrap_err();
            assert_eq!(err.exit_code(), 5, "{status}");
            assert!(err.to_string().contains("nothing was tapped"), "{err}");
            assert!(fake.taps.borrow().is_empty(), "{status}");
        }
    }

    #[test]
    fn two_doors_read_but_neither_gets_a_tap() {
        let page = two_doors();
        let Screen::Doors(cards) = parse_screen(&page) else {
            panic!("expected doors");
        };
        assert_eq!(cards.len(), 2);

        let renamed = page.replacen("Dummy Garage", "Dummy Other", 1);
        let fake = Fake::new(&[renamed]);
        let err = set(&fake, DoorCommand::Open).unwrap_err();
        assert!(err.to_string().contains("nothing was tapped"), "{err}");
        assert!(fake.taps.borrow().is_empty());
    }

    #[test]
    fn a_door_with_no_usable_button_gets_no_tap() {
        for page in [
            HOME.replace("resource-id=\"device_state_icon\"", "resource-id=\"other\""),
            HOME.replace(
                "resource-id=\"device_state_icon\" class=\"android.view.View\" package=\"com.chamberlain.android.liftmaster.myq\" content-desc=\"\" clickable=\"true\" enabled=\"true\"",
                "resource-id=\"device_state_icon\" class=\"android.view.View\" package=\"com.chamberlain.android.liftmaster.myq\" content-desc=\"\" clickable=\"true\" enabled=\"false\"",
            ),
        ] {
            let fake = Fake::new(&[page]);
            let err = set(&fake, DoorCommand::Open).unwrap_err();
            assert!(err.to_string().contains("nothing was tapped"), "{err}");
            assert!(fake.taps.borrow().is_empty());
        }
    }

    #[test]
    fn another_apps_screen_gets_no_tap() {
        let fake = Fake::new(&[other_app()]);
        assert!(set(&fake, DoorCommand::Open).is_err());
        assert!(fake.taps.borrow().is_empty());
    }

    #[test]
    fn a_door_that_never_arrives_is_reported_and_never_tapped_again() {
        let fake = Fake::new(&[HOME.to_string(), reading("Opening")]);
        let err = set(&fake, DoorCommand::Open).unwrap_err();
        assert_eq!(err.exit_code(), 5);
        let text = err.to_string();
        assert!(text.contains("was tapped") && text.contains("Check the door"));
        assert!(text.contains("Opening"), "{text}");
        assert_eq!(fake.taps.borrow().len(), 1);
    }

    #[test]
    fn a_failed_tap_says_the_door_may_be_moving_and_is_not_retried() {
        let mut fake = Fake::new(&[HOME.to_string()]);
        fake.tap_fails = true;
        let err = set(&fake, DoorCommand::Open).unwrap_err();
        assert_eq!(err.exit_code(), 5);
        let text = err.to_string();
        assert!(text.contains("may be moving") && text.contains("adb did not answer"));
        assert_eq!(fake.taps.borrow().len(), 1);
    }

    #[test]
    fn the_wait_ends_at_its_deadline_however_slow_the_reads() {
        let mut fake = Fake::new(&[HOME.to_string(), reading("Opening")]);
        fake.read_delay = Duration::from_millis(20);
        let wait = Wait {
            timeout: Duration::from_millis(30),
            interval: Duration::from_millis(1),
        };
        set_door(&fake, "garage", DoorCommand::Open, wait, &|_| {}).unwrap_err();
        assert!(fake.reads.get() <= 5, "{} reads", fake.reads.get());
    }

    #[test]
    fn a_stalled_door_that_got_no_tap_does_not_claim_one() {
        let fake = Fake::new(&[reading("Opening")]);
        let err = set(&fake, DoorCommand::Open).unwrap_err();
        assert!(err
            .to_string()
            .starts_with("network/upstream error: Nothing was tapped"));
        assert!(fake.taps.borrow().is_empty());
    }

    #[test]
    fn a_screen_lost_during_travel_is_waited_out() {
        let fake = Fake::new(&[
            HOME.to_string(),
            "ERROR: could not get idle state.".to_string(),
            other_app(),
            reading("Open"),
        ]);
        let change = set(&fake, DoorCommand::Open).unwrap();
        assert!(change.confirmed);
        assert_eq!(fake.taps.borrow().len(), 1);
    }

    #[test]
    fn no_wait_reads_once_and_claims_nothing_it_did_not_see() {
        let wait = Wait {
            timeout: Duration::ZERO,
            interval: Duration::from_millis(10),
        };
        let fake = Fake::new(&[HOME.to_string(), reading("Opening")]);
        let change = set_door(&fake, "garage", DoorCommand::Open, wait, &|_| {}).unwrap();
        assert!(change.changed && !change.confirmed);
        assert_eq!(change.door.state, "opening");
    }

    #[test]
    fn no_wait_with_a_lost_read_does_not_repeat_the_old_state() {
        let wait = Wait {
            timeout: Duration::ZERO,
            interval: Duration::from_millis(10),
        };
        let fake = Fake::new(&[HOME.to_string(), other_app()]);
        let change = set_door(&fake, "garage", DoorCommand::Open, wait, &|_| {}).unwrap();
        assert!(change.changed && !change.confirmed);
        assert_eq!(change.door.state, "unknown");
        assert_eq!(change.door.status, None);
    }

    #[test]
    fn adb_failures_say_what_to_do() {
        for (stderr, code, expected) in [
            ("error: no devices/emulators found", 5, "no phone"),
            ("error: device offline", 5, "no phone"),
            ("error: device 'x' not found", 5, "no phone"),
            ("error: device unauthorized.", 3, "USB debugging"),
            ("error: more than one device/emulator", 2, "ANDROID_SERIAL"),
            ("error: dummy reason", 5, "dummy reason"),
            (
                "Error: Activity class {dummy/dummy} does not exist.",
                5,
                "adb failed",
            ),
            ("/system/bin/sh: input: not found", 5, "adb failed"),
        ] {
            let err = adb_failure(stderr);
            assert_eq!(err.exit_code(), code, "{stderr}");
            assert!(err.to_string().contains(expected), "{err}");
        }
    }

    #[test]
    fn a_missing_adb_names_what_to_install() {
        let adb = Adb {
            program: "/nonexistent/adb".into(),
        };
        let err = adb.screen_xml().unwrap_err();
        assert_eq!(err.exit_code(), 5);
        assert!(err.to_string().contains("platform-tools"), "{err}");
    }
}
