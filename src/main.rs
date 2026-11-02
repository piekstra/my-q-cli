//! `myq`: control your own myQ garage door from the command line and from MCP
//! clients. Conforms to piekstra-cli/1. Unofficial.

use std::time::Duration;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use pk_cli_auth::otp::{CodeArgs, OtpLogin};
use pk_cli_auth::{AuthMethod, AuthStatus, LoginArgs, LogoutArgs, SetCredentialArgs};
use pk_cli_config::ConfigStore;
use pk_cli_core::info::{AuthInfo, CliInfo};
use pk_cli_core::{block_on, confirm, dates, output, CliError, CommonArgs};
use pk_cli_http::{ApiArgs, BlockingTransport};
use pk_cli_secrets::{OnePassword, OpArgs, OpRef, Secret};
use pk_cli_selfupdate::{SelfUpdateArgs, Updater};
use serde_json::{json, Value};

use myq::auth::{self, SignIn};
use myq::client::{self, unix_now, Client, Wait, POLL_INTERVAL};
use myq::config::Config;
use myq::credential::{Credentials, KeychainStore, ParkedView, TokenStore, ENV_EMAIL};
use myq::door::{DoorChange, DoorCommand};
use myq::{hub, mcp, BIN, VERSION};

const REPO: &str = "piekstra/my-q-cli";

/// Control your own myQ garage door from the command line. Unofficial.
#[derive(Parser, Debug)]
#[command(name = BIN, version, about, long_about = None)]
struct Cli {
    #[command(flatten)]
    common: CommonArgs,

    /// The myQ account to act on, when the login has more than one (id or name).
    #[arg(short, long, global = true, env = "MYQ_ACCOUNT")]
    account: Option<String>,

    /// Override the config file location.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Sign in, sign out, and report the stored credential.
    #[command(subcommand)]
    Auth(AuthCmd),
    /// Non-secret settings.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// The myQ accounts this login can act on.
    #[command(subcommand)]
    Accounts(AccountsCmd),
    /// Every device on the account: doors, hubs, lamps.
    #[command(subcommand)]
    Devices(DevicesCmd),
    /// Garage doors: read their state, open and close them.
    #[command(subcommand)]
    Doors(DoorsCmd),
    /// The opener's own Wi-Fi status, read directly over the home network.
    #[command(subcommand)]
    Hub(HubCmd),
    /// Raw myQ API passthrough. Anything but GET needs confirmation.
    Api(RawArgs),
    /// Serve door tools to Claude and other MCP clients over stdio.
    Mcp(pk_cli_mcp::McpArgs),
    /// Update to the latest release from GitHub.
    SelfUpdate(SelfUpdateArgs),
    /// Print a shell completion script.
    Completions { shell: Shell },
    /// Machine-readable capability discovery (cli-info/v1).
    Info,
}

#[derive(Subcommand, Debug)]
enum AuthCmd {
    /// Sign in with your myQ email and password and keep the session in the OS keychain.
    Login(LoginCmd),
    /// Report the stored credential and session (auth-status/v1). Makes no request.
    Status,
    /// Clear the session; --forget also removes the password and saved identity.
    Logout(LogoutArgs),
    /// Replace the stored password without signing in, for rotation or headless setup.
    SetCredential(SetCredentialCmd),
}

#[derive(clap::Args, Debug)]
struct LoginCmd {
    #[command(flatten)]
    login: LoginArgs,
    #[command(flatten)]
    op: OpArgs,
    /// The myQ email. Falls back to MYQ_EMAIL, then the config's `email`.
    #[arg(long)]
    email: Option<String>,
    /// Read the email from 1Password (a secret reference, not the email itself).
    #[arg(long, value_name = "op://VAULT/ITEM/FIELD", conflicts_with = "email")]
    email_op: Option<OpRef>,
    #[command(flatten)]
    code: CodeArgs,
}

#[derive(clap::Args, Debug)]
struct SetCredentialCmd {
    #[command(flatten)]
    args: SetCredentialArgs,
    #[command(flatten)]
    op: OpArgs,
    /// The myQ email. Falls back to MYQ_EMAIL, then the config's `email`.
    #[arg(long)]
    email: Option<String>,
}

#[derive(Subcommand, Debug)]
enum ConfigCmd {
    /// Print the resolved config file path.
    Path,
    /// Show the effective configuration.
    Show,
    /// Set a config key (e.g. `config set hub_host 192.0.2.10`).
    Set { key: String, value: String },
    /// Remove a config key.
    Unset { key: String },
}

#[derive(Subcommand, Debug)]
enum AccountsCmd {
    /// List accounts (account-list/v1).
    #[command(visible_alias = "ls")]
    List,
    /// Act on this account by default (id or name).
    Use { account: String },
}

#[derive(Subcommand, Debug)]
enum DevicesCmd {
    /// List every device on the account (device-list/v1).
    #[command(visible_alias = "ls")]
    List,
}

#[derive(clap::Args, Debug)]
struct MoveArgs {
    /// Door name, serial number, or a unique part of its name.
    door: String,
    /// Skip the confirmation prompt (required when non-interactive).
    #[arg(long)]
    force: bool,
    /// Send the command and read once, without waiting for the door to arrive.
    #[arg(long, conflicts_with = "timeout")]
    no_wait: bool,
    /// Seconds to wait for a read-back to show the door arrived.
    #[arg(long, value_name = "SECONDS", default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=600))]
    timeout: u64,
}

#[derive(Subcommand, Debug)]
enum DoorsCmd {
    /// List garage doors and their state (door-list/v1).
    #[command(visible_alias = "ls")]
    List,
    /// Show one door (door/v1).
    Get {
        /// Door name, serial number, or a unique part of its name.
        door: String,
    },
    /// Open a door, and report what a read-back shows (door-change/v1).
    Open(MoveArgs),
    /// Close a door, and report what a read-back shows (door-change/v1).
    Close(MoveArgs),
}

#[derive(Subcommand, Debug)]
enum HubCmd {
    /// Read Wi-Fi status from the opener itself (hub-status/v1). Needs no sign-in.
    Status {
        /// The opener's address on your network. Falls back to MYQ_HUB_HOST,
        /// then the config's `hub_host`.
        #[arg(long)]
        host: Option<String>,
    },
}

#[derive(clap::Args, Debug)]
struct RawArgs {
    #[command(flatten)]
    api: ApiArgs,
    /// The service a relative path belongs to.
    #[arg(long, value_enum, default_value_t = Service::Devices)]
    service: Service,
    /// Skip the confirmation prompt for anything but GET (required when non-interactive).
    #[arg(long)]
    force: bool,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum Service {
    Accounts,
    Devices,
    Gdo,
    Identity,
}

impl Service {
    fn base(self) -> &'static str {
        match self {
            Service::Accounts => client::ACCOUNTS,
            Service::Devices => client::DEVICES,
            Service::Gdo => client::GDO,
            Service::Identity => auth::IDS,
        }
    }
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(&cli) {
        std::process::exit(output::fail(&e, cli.common.json));
    }
}

fn config_store(cli: &Cli) -> ConfigStore {
    ConfigStore::new(BIN).with_override(cli.config.clone())
}

fn cloud() -> Result<Client<BlockingTransport, KeychainStore>, CliError> {
    Ok(Client::new(BlockingTransport::new()?, KeychainStore::new()))
}

fn preferred_account(cli: &Cli, cfg: &Config) -> Option<String> {
    cli.account.clone().or_else(|| cfg.account.clone())
}

fn run(cli: &Cli) -> Result<(), CliError> {
    let store = config_store(cli);
    let json = cli.common.json;
    match &cli.command {
        Command::Auth(cmd) => auth_cmd(cli, cmd, &store),
        Command::Config(cmd) => config_cmd(cli, cmd, &store),
        Command::Accounts(cmd) => accounts_cmd(cli, cmd, &store),
        Command::Devices(DevicesCmd::List) => {
            let cfg: Config = store.load()?;
            let client = cloud()?;
            let account = block_on(client.account_id(preferred_account(cli, &cfg).as_deref()))?;
            let rows = values(&block_on(client.devices(&account))?)?;
            output::emit_list(
                json,
                "device",
                rows,
                &[
                    "name",
                    "family",
                    "device_type",
                    "online",
                    "door_state",
                    "lamp_state",
                    "serial",
                ],
            );
            Ok(())
        }
        Command::Doors(cmd) => doors_cmd(cli, cmd, &store),
        Command::Hub(HubCmd::Status { host }) => {
            let cfg: Config = store.load()?;
            let host = host
                .clone()
                .or_else(|| std::env::var(hub::ENV_HOST).ok().filter(|h| !h.is_empty()))
                .or(cfg.hub_host)
                .ok_or_else(|| {
                    CliError::Usage(format!(
                        "no hub address: pass --host, set {}, or `{BIN} config set hub_host <address>`",
                        hub::ENV_HOST
                    ))
                })?;
            hub::validate_host(&host)?;
            let status = block_on(hub::status(&BlockingTransport::new()?, &host))?;
            output::emit_one(json, "hub-status", to_value(&status)?);
            Ok(())
        }
        Command::Api(args) => raw_cmd(cli, args),
        Command::Mcp(args) => {
            let cfg: Config = store.load()?;
            mcp::serve(args, preferred_account(cli, &cfg))
        }
        Command::SelfUpdate(args) => Updater {
            repo: REPO.into(),
            binary: BIN.into(),
            target: env!("BUILD_TARGET").into(),
            current: VERSION.into(),
        }
        .run(args, json, cli.common.quiet),
        Command::Completions { shell } => {
            clap_complete::generate(*shell, &mut Cli::command(), BIN, &mut std::io::stdout());
            Ok(())
        }
        Command::Info => {
            let info = CliInfo::new(
                BIN,
                VERSION,
                &format!("https://github.com/{REPO}"),
                AuthInfo {
                    required: true,
                    method: "password".into(),
                    login_hint: Some(format!("{BIN} auth login")),
                },
                &["accounts", "devices", "doors", "hub", "api", "mcp"],
            );
            output::json(&serde_json::to_value(&info).expect("CliInfo serializes to JSON"));
            Ok(())
        }
    }
}

fn to_value<T: serde::Serialize>(value: &T) -> Result<Value, CliError> {
    serde_json::to_value(value).map_err(|e| CliError::Other(format!("serializing output: {e}")))
}

fn values<T: serde::Serialize>(items: &[T]) -> Result<Vec<Value>, CliError> {
    items.iter().map(to_value).collect()
}

/// The email from the flag, 1Password, the environment or the config, in that
/// order. Errors before any password is read.
fn resolve_email(flag: Option<&str>, op: Option<&OpRef>, cfg: &Config) -> Result<String, CliError> {
    if let Some(reference) = op {
        return Ok(OnePassword::new().read(reference)?.expose().to_string());
    }
    flag.map(str::to_string)
        .or_else(|| std::env::var(ENV_EMAIL).ok().filter(|v| !v.is_empty()))
        .or_else(|| cfg.email.clone())
        .ok_or_else(|| {
            CliError::Usage(format!(
                "no email: pass --email or --email-op, set {ENV_EMAIL}, or `{BIN} config set email <address>`"
            ))
        })
}

fn read_password(
    source: &pk_cli_secrets::SecretSourceArgs,
    op: &OpArgs,
    cfg: &Config,
    prompt: Option<&str>,
) -> Result<Secret, CliError> {
    let explicit = source.stdin || source.from_env.is_some();
    let op = match (&op.op, &cfg.op_ref) {
        (None, Some(reference)) if !explicit => OpArgs::reference(reference.clone()),
        _ => op.clone(),
    };
    source.read_with_op(&op, &OnePassword::new(), prompt)
}

fn auth_cmd(cli: &Cli, cmd: &AuthCmd, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    let cfg: Config = store.load()?;
    let keychain = KeychainStore::new();
    match cmd {
        AuthCmd::Login(LoginCmd {
            login,
            op,
            email,
            email_op,
            code,
        }) => {
            let code = code.resolve()?;
            let interactive = cli.common.interactive() && !login.non_interactive;
            let transport = BlockingTransport::new()?;
            let mut credentials;
            if let Some(code) = &code {
                if login.source.stdin
                    || login.source.from_env.is_some()
                    || login.no_verify
                    || op.op.is_some()
                    || email.is_some()
                    || email_op.is_some()
                {
                    return Err(CliError::Usage(
                        "--code finishes the sign-in that is waiting; it takes no email or \
                         password"
                            .into(),
                    ));
                }
                let signin = SignIn::resume(&transport);
                let slot = ParkedView::new(&keychain, None);
                let flow = OtpLogin::new(BIN, &signin, &slot)
                    .interactive(interactive)
                    .quiet(cli.common.quiet);
                let (tokens, code_required) = signin.finish(&flow, Some(code))?;
                credentials = keychain.load_item()?.unwrap_or_default();
                store_tokens(&mut credentials, tokens, code_required);
            } else {
                let email_ref = email_op.as_ref().or(cfg.op_email_ref.as_ref());
                let use_ref = if email.is_some() { None } else { email_ref };
                let email = resolve_email(email.as_deref(), use_ref, &cfg)?;
                let existing = keychain.load_item()?;
                let in_the_way = if login.no_verify {
                    existing.is_some()
                } else {
                    existing
                        .as_ref()
                        .is_some_and(|c| c.access_token.is_some() || c.refresh_token.is_some())
                };
                if in_the_way && !login.overwrite {
                    return Err(CliError::Usage(
                        "a credential is already stored; pass --overwrite to replace it".into(),
                    ));
                }
                let prompt = (!login.non_interactive).then_some("myQ password");
                let password = read_password(&login.source, op, &cfg, prompt)?;
                credentials = Credentials {
                    email: email.clone(),
                    password: Some(password.expose().to_string()),
                    ..Credentials::default()
                };
                let save_config = |email: &str| -> Result<(), CliError> {
                    let mut saved: Config = store.load()?;
                    saved.email = Some(email.to_string());
                    if let Some(reference) = email_op {
                        saved.op_email_ref = Some(reference.clone());
                    }
                    if let Some(reference) = &op.op {
                        saved.op_ref = Some(reference.clone());
                    }
                    store.save(&saved)
                };
                if !login.no_verify {
                    let signin = SignIn::start(&transport, &email, password.expose());
                    let slot = ParkedView::new(&keychain, Some(credentials.clone()));
                    let flow = OtpLogin::new(BIN, &signin, &slot)
                        .interactive(interactive)
                        .quiet(cli.common.quiet);
                    let (tokens, code_required) = match signin.finish(&flow, None) {
                        Ok(done) => done,
                        Err(err) => {
                            // A sign-in parked on its code finishes in a later run.
                            if signin.reached_code_step() {
                                save_config(&email)?;
                            }
                            return Err(err);
                        }
                    };
                    store_tokens(&mut credentials, tokens, code_required);
                }
                save_config(&email)?;
            }
            keychain.save(&credentials)?;
            let saved: Config = store.load()?;
            if !cli.common.quiet {
                eprintln!(
                    "{} stored in the OS keychain",
                    if login.no_verify {
                        "credential (not verified)"
                    } else {
                        "signed in; session"
                    }
                );
            }
            status_of(&keychain, &saved)?.emit(json);
            Ok(())
        }
        AuthCmd::Status => {
            if !cli.common.quiet && keychain.load_item()?.is_some_and(|c| c.pending.is_some()) {
                eprintln!("a sign-in is waiting for its code: `{BIN} auth login --code <CODE>`");
            }
            status_of(&keychain, &cfg)?.emit(json);
            Ok(())
        }
        AuthCmd::Logout(args) => {
            if args.forget {
                keychain.delete()?;
                store.clear()?;
            } else if let Some(mut credentials) = keychain.load_item()? {
                credentials.access_token = None;
                credentials.refresh_token = None;
                credentials.expires_at = None;
                credentials.pending = None;
                keychain.save(&credentials)?;
            }
            if !cli.common.quiet {
                eprintln!("signed out");
            }
            Ok(())
        }
        AuthCmd::SetCredential(SetCredentialCmd { args, op, email }) => {
            let email = resolve_email(email.as_deref(), None, &cfg)?;
            let existing = keychain.load_item()?;
            if existing.is_some() && !args.overwrite {
                return Err(CliError::Usage(
                    "a credential is already stored; pass --overwrite to replace it".into(),
                ));
            }
            let password = read_password(&args.source, op, &cfg, None)?;
            keychain.save(&Credentials {
                email,
                password: Some(password.expose().to_string()),
                ..Credentials::default()
            })?;
            if !cli.common.quiet {
                eprintln!("credential stored; the old session was cleared");
            }
            Ok(())
        }
    }
}

fn store_tokens(credentials: &mut Credentials, tokens: auth::Tokens, code_required: bool) {
    credentials.access_token = Some(tokens.access_token);
    credentials.refresh_token = tokens.refresh_token;
    credentials.expires_at = tokens.expires_in.map(|s| unix_now() + s);
    credentials.code_required = code_required;
    credentials.pending = None;
}

fn status_of(keychain: &KeychainStore, cfg: &Config) -> Result<AuthStatus, CliError> {
    let from_env = KeychainStore::from_env();
    let item = if from_env.is_some() {
        None
    } else {
        keychain.load_item()?
    };
    let credentials = from_env.as_ref().or(item.as_ref());
    let live = credentials
        .and_then(|c| c.expires_at.filter(|_| c.access_token.is_some()))
        .is_some_and(|at| at > unix_now());
    let renewable = credentials
        .is_some_and(|c| c.refresh_token.is_some() || (c.password.is_some() && !c.code_required));
    let mut status = AuthStatus::new(true, live || renewable, AuthMethod::Password);
    status.username = credentials
        .map(|c| c.email.clone())
        .or_else(|| cfg.email.clone());
    status.account = cfg.account.clone();
    status.credential_in_keychain = Some(item.is_some());
    status.session_valid = Some(live);
    status.expires_at = credentials
        .and_then(|c| c.expires_at)
        .map(|at| dates::fmt_rfc3339(at as i64));
    Ok(status)
}

fn config_cmd(cli: &Cli, cmd: &ConfigCmd, store: &ConfigStore) -> Result<(), CliError> {
    match cmd {
        ConfigCmd::Path => {
            println!("{}", store.path()?.display());
            Ok(())
        }
        ConfigCmd::Show => {
            let cfg: Config = store.load()?;
            let value = to_value(&cfg)?;
            if cli.common.json {
                output::json(&value);
            } else {
                output::render(&value);
            }
            Ok(())
        }
        ConfigCmd::Set { key, value } => {
            let mut cfg: Config = store.load()?;
            cfg.set(key, value)?;
            store.save(&cfg)
        }
        ConfigCmd::Unset { key } => {
            let mut cfg: Config = store.load()?;
            cfg.unset(key)?;
            store.save(&cfg)
        }
    }
}

fn accounts_cmd(cli: &Cli, cmd: &AccountsCmd, store: &ConfigStore) -> Result<(), CliError> {
    let client = cloud()?;
    match cmd {
        AccountsCmd::List => {
            let rows = values(&block_on(client.accounts())?)?;
            output::emit_list(cli.common.json, "account", rows, &["name", "id"]);
            Ok(())
        }
        AccountsCmd::Use { account } => {
            let id = block_on(client.account_id(Some(account)))?;
            let mut cfg: Config = store.load()?;
            cfg.account = Some(id.clone());
            store.save(&cfg)?;
            output::emit_one(cli.common.json, "account", json!({ "id": id }));
            Ok(())
        }
    }
}

fn doors_cmd(cli: &Cli, cmd: &DoorsCmd, store: &ConfigStore) -> Result<(), CliError> {
    let json = cli.common.json;
    // The gate comes first: a driver that forgot --force exits 6 before any
    // keychain or network work (SPEC §1.3).
    if let DoorsCmd::Open(args) | DoorsCmd::Close(args) = cmd {
        confirm::require_confirmable(args.force, cli.common.interactive(), "moving a garage door")?;
    }
    let cfg: Config = store.load()?;
    let client = cloud()?;
    let account = block_on(client.account_id(preferred_account(cli, &cfg).as_deref()))?;
    match cmd {
        DoorsCmd::List => {
            let rows = values(&block_on(client.doors(&account))?)?;
            output::emit_list(
                json,
                "door",
                rows,
                &["name", "state", "online", "last_update", "serial"],
            );
            Ok(())
        }
        DoorsCmd::Get { door } => {
            let doors = block_on(client.doors(&account))?;
            output::emit_one(json, "door", to_value(myq::door::find(&doors, door)?)?);
            Ok(())
        }
        DoorsCmd::Open(args) => move_door(cli, &client, &account, args, DoorCommand::Open),
        DoorsCmd::Close(args) => move_door(cli, &client, &account, args, DoorCommand::Close),
    }
}

fn move_door(
    cli: &Cli,
    client: &Client<BlockingTransport, KeychainStore>,
    account: &str,
    args: &MoveArgs,
    command: DoorCommand,
) -> Result<(), CliError> {
    let doors = block_on(client.doors(account))?;
    let door = myq::door::find(&doors, &args.door)?;
    if door.state != command.target() {
        confirm::confirm(
            args.force,
            &format!(
                "{} `{}`, which reads `{}`? This moves the real door.",
                match command {
                    DoorCommand::Open => "Open",
                    DoorCommand::Close => "Close",
                },
                door.name,
                door.state
            ),
        )?;
    }
    let wait = Wait {
        timeout: if args.no_wait {
            Duration::ZERO
        } else {
            Duration::from_secs(args.timeout)
        },
        interval: POLL_INTERVAL,
    };
    let change: DoorChange =
        block_on(client.set_door(account, &door.serial, command, wait, &std::thread::sleep))?;
    output::emit_one(
        cli.common.json,
        "door-change",
        json!({
            "name": change.door.name,
            "state": change.door.state,
            "changed": change.changed,
            "confirmed": change.confirmed,
            "serial": change.door.serial,
        }),
    );
    Ok(())
}

fn raw_cmd(cli: &Cli, args: &RawArgs) -> Result<(), CliError> {
    let method = args.api.parsed_method()?;
    let body = args.api.parsed_body()?;
    let url = args.api.url(args.service.base());
    client::ensure_myq_host(&url)?;
    if method != "GET" {
        confirm::gate(
            &cli.common,
            args.force,
            &format!("Send {method} to myQ? This can change your account or move a door."),
        )?;
    }
    let client = cloud()?;
    let value = block_on(client.raw(method.as_str(), &url, body.map(|b| b.to_string())))?;
    output::json(&value);
    Ok(())
}
