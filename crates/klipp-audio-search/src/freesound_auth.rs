//! Signing in to the user's own Freesound account (#1894), so a sound
//! effect can be downloaded as its original recording rather than the
//! preview MP3 anyone may fetch.
//!
//! Freesound serves originals only to an OAuth2 bearer token. The client
//! id and secret are the user's own, from an API credential they make at
//! freesound.org/apiv2/apply with its callback URL set to
//! [`REDIRECT_URI`]; they live in `~/.config/klipp/keys.toml`
//! (`freesound_client_id`, `freesound_client_secret`) or the
//! `KLIPP_FREESOUND_CLIENT_ID` / `KLIPP_FREESOUND_CLIENT_SECRET`
//! variables, never in the build. Signing in opens the browser at the
//! authorize page; Freesound sends it back to a one-request listener on
//! [`REDIRECT_URI`], which takes the code. Where that can't be had (the
//! port is taken, or the credential's callback is Freesound's own code
//! page) the code shown is pasted instead. The tokens are kept in
//! `~/.config/klipp/freesound-oauth.json`, readable by the user alone, and
//! refreshed when they lapse. A token is never logged or put in an error.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

/// The port the sign-in listens on for Freesound's redirect: fixed, as the
/// credential's callback URL has to name it exactly.
pub const REDIRECT_PORT: u16 = 47_861;
/// The callback URL to give the Freesound credential.
pub const REDIRECT_URI: &str = "http://127.0.0.1:47861/freesound/callback";

const AUTHORIZE: &str = "https://freesound.org/apiv2/oauth2/authorize/";
const TOKEN: &str = "https://freesound.org/apiv2/oauth2/access_token/";

/// The user's Freesound API credential.
#[derive(Debug, Clone)]
pub struct Client {
    pub id: String,
    secret: String,
}

impl Client {
    /// The credential from the user's keys, if they set one.
    pub fn from_keys() -> Option<Self> {
        Some(Self {
            id: crate::read_key("KLIPP_FREESOUND_CLIENT_ID", "freesound_client_id")?,
            secret: crate::read_key("KLIPP_FREESOUND_CLIENT_SECRET", "freesound_client_secret")?,
        })
    }

    /// The page the person approves the sign-in on; `state` comes back with
    /// the code, so a redirect another page made is not taken for it.
    pub fn authorize_url(&self, state: &str) -> String {
        format!("{AUTHORIZE}?client_id={}&response_type=code&state={}&redirect_uri={}", crate::encode(&self.id), crate::encode(state), crate::encode(REDIRECT_URI))
    }
}

/// What signing in keeps: the tokens, when the access one lapses, and
/// whose account it is.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Tokens {
    access_token: String,
    refresh_token: String,
    /// Seconds since the epoch.
    pub expires_at: u64,
    #[serde(default)]
    pub username: Option<String>,
}

impl Tokens {
    /// Whether the access token has lapsed, or does within a minute.
    pub fn lapsed(&self, now: u64) -> bool {
        now + 60 >= self.expires_at
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Where the tokens are kept.
pub fn token_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("klipp").join("freesound-oauth.json"))
}

/// Write `tokens` to `path`, readable and writable by the user alone.
pub fn save_at(path: &std::path::Path, tokens: &Tokens) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(tokens).map_err(|e| e.to_string())?;
    let part = path.with_extension("json.part");
    // A fresh file each time, so its mode is the one set here and not a
    // leftover's.
    let _ = std::fs::remove_file(&part);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    file.write_all(text.as_bytes()).map_err(|e| format!("{}: {e}", part.display()))?;
    drop(file);
    std::fs::rename(&part, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// The tokens kept at `path`, if any.
pub fn load_at(path: &std::path::Path) -> Option<Tokens> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Forget the sign-in: the token file goes.
pub fn sign_out() -> Result<(), String> {
    let Some(path) = token_path() else { return Ok(()) };
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {e}", path.display())),
        _ => Ok(()),
    }
}

/// Who is signed in, without asking the network: the kept tokens, if a
/// credential is set to refresh them with.
pub fn signed_in() -> Option<Tokens> {
    Client::from_keys()?;
    load_at(&token_path()?)
}

/// A state value for one sign-in: unguessable enough that another page's
/// redirect to the listener isn't taken for this one.
pub fn new_state() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(now());
    h.write_u32(std::process::id());
    format!("{:016x}", h.finish())
}

/// The code in a redirect's request line, if its state is `state`: the
/// path and query of `GET /freesound/callback?code=…&state=… HTTP/1.1`.
/// `Err` for a redirect that says the person declined, or a state that
/// isn't this sign-in's.
pub fn code_from_request(line: &str, state: &str) -> Result<String, String> {
    let target = line.split_whitespace().nth(1).ok_or("not a request")?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/freesound/callback" {
        return Err(format!("not the callback: {path}"));
    }
    let field = |k: &str| query.split('&').find_map(|kv| kv.strip_prefix(&format!("{k}="))).map(crate::decode);
    if let Some(e) = field("error") {
        return Err(format!("Freesound said {e}: the sign-in was not approved"));
    }
    if field("state").as_deref() != Some(state) {
        return Err("the redirect was not for this sign-in".into());
    }
    field("code").filter(|c| !c.is_empty()).ok_or_else(|| "the redirect carried no code".into())
}

/// Wait up to `timeout` for Freesound's redirect on [`REDIRECT_PORT`], and
/// answer the browser. `Err` when the port can't be had (another program
/// holds it) or nothing came: then the code is pasted.
pub fn await_redirect(state: &str, timeout: std::time::Duration) -> Result<String, String> {
    await_redirect_signalling(state, timeout, || {})
}

/// [`await_redirect`], calling `listening` once the port is held, so the
/// browser is sent to the approval page only when the redirect has
/// somewhere to come back to.
pub fn await_redirect_signalling(state: &str, timeout: std::time::Duration, listening: impl FnOnce()) -> Result<String, String> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", REDIRECT_PORT)).map_err(|e| format!("could not listen on {REDIRECT_URI}: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    listening();
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).map_err(|e| e.to_string())?;
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                reader.read_line(&mut line).map_err(|e| e.to_string())?;
                let got = code_from_request(&line, state);
                let page = match &got {
                    Ok(_) => "Signed in to Freesound. You can close this tab and return to Klipp.",
                    Err(_) => "Klipp could not take this sign-in. Return to Klipp and try again.",
                };
                let mut out = &stream;
                let _ = write!(out, "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len());
                // A stray request (a favicon) waits for the real one.
                match got {
                    Err(e) if e.starts_with("not the callback") => {}
                    other => return other,
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err("no sign-in came back from the browser in time".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// A reply's body as JSON.
fn json_of(resp: ureq::Response) -> Option<serde_json::Value> {
    serde_json::from_str(&resp.into_string().ok()?).ok()
}

/// The tokens a token reply gives; the network error or Freesound's
/// reason otherwise, never the request's secret.
fn tokens_from(resp: Result<ureq::Response, ureq::Error>, previous_name: Option<String>) -> Result<Tokens, String> {
    let resp = match resp {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let why = json_of(r).and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned)).unwrap_or_default();
            return Err(format!("Freesound refused the sign-in ({code} {why})"));
        }
        Err(e) => return Err(format!("Freesound could not be reached: {}", e.kind())),
    };
    let v = json_of(resp).ok_or("Freesound's token reply was unreadable")?;
    let field = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_owned).ok_or_else(|| format!("Freesound's token reply had no {k}"));
    Ok(Tokens {
        access_token: field("access_token")?,
        refresh_token: field("refresh_token")?,
        expires_at: now() + v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(86_400),
        username: previous_name,
    })
}

/// Trade the code for tokens, learn whose account it is, and keep them.
pub fn finish_sign_in(client: &Client, code: &str) -> Result<Tokens, String> {
    let resp = ureq::post(TOKEN).timeout(std::time::Duration::from_secs(30)).send_form(&[
        ("client_id", client.id.as_str()),
        ("client_secret", client.secret.as_str()),
        ("grant_type", "authorization_code"),
        ("code", code.trim()),
        ("redirect_uri", REDIRECT_URI),
    ]);
    let mut tokens = tokens_from(resp, None)?;
    tokens.username = ureq::get("https://freesound.org/apiv2/me/")
        .set("Authorization", &format!("Bearer {}", tokens.access_token))
        .timeout(std::time::Duration::from_secs(20))
        .call()
        .ok()
        .and_then(json_of)
        .and_then(|v| v.get("username").and_then(|u| u.as_str()).map(str::to_owned));
    save_at(&token_path().ok_or("no config folder to keep the sign-in in")?, &tokens)?;
    Ok(tokens)
}

/// A bearer token that is good now: the kept one, refreshed first when it
/// has lapsed. `Err` when signed out, or the refresh is refused (then the
/// sign-in is forgotten and has to be done again).
pub fn access_token() -> Result<String, String> {
    let client = Client::from_keys().ok_or("no Freesound credential: put freesound_client_id and freesound_client_secret in ~/.config/klipp/keys.toml")?;
    let path = token_path().ok_or("no config folder")?;
    let tokens = load_at(&path).ok_or("not signed in to Freesound")?;
    if !tokens.lapsed(now()) {
        return Ok(tokens.access_token);
    }
    let resp = ureq::post(TOKEN).timeout(std::time::Duration::from_secs(30)).send_form(&[
        ("client_id", client.id.as_str()),
        ("client_secret", client.secret.as_str()),
        ("grant_type", "refresh_token"),
        ("refresh_token", tokens.refresh_token.as_str()),
    ]);
    match tokens_from(resp, tokens.username.clone()) {
        Ok(fresh) => {
            save_at(&path, &fresh)?;
            Ok(fresh.access_token)
        }
        Err(e) => {
            let _ = sign_out();
            Err(format!("the Freesound sign-in lapsed and could not be renewed ({e}); sign in again"))
        }
    }
}

/// Open `url` in the person's browser.
pub fn open_browser(url: &str) -> Result<(), String> {
    std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not open the browser: {e}"))
}

#[cfg(test)]
mod tests {

    /// The token file is the user's alone even when a leftover partial
    /// file was readable by others.
    #[cfg(unix)]
    #[test]
    fn the_token_file_is_private_whatever_was_left() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("klipp-token-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("freesound.json");
        let part = path.with_extension("json.part");
        std::fs::write(&part, b"old").unwrap();
        std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o644)).unwrap();
        save_at(&path, &Tokens { access_token: "a".into(), refresh_token: "r".into(), expires_at: 0, username: None }).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    fn tokens() -> Tokens {
        Tokens { access_token: "a".into(), refresh_token: "r".into(), expires_at: 1_000, username: Some("dan".into()) }
    }

    #[test]
    fn the_authorize_page_names_the_credential_the_state_and_the_callback() {
        let c = Client { id: "abc123".into(), secret: "s".into() };
        let url = c.authorize_url("st4te");
        assert!(url.starts_with(AUTHORIZE), "{url}");
        assert!(url.contains("client_id=abc123") && url.contains("state=st4te") && url.contains("response_type=code"), "{url}");
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A47861%2Ffreesound%2Fcallback"), "{url}");
        assert!(!url.contains("secret") && !url.contains("=s&"), "the secret stays out: {url}");
        assert_eq!(REDIRECT_URI, format!("http://127.0.0.1:{REDIRECT_PORT}/freesound/callback"));
    }

    /// The code is taken from the redirect only with this sign-in's state;
    /// a declined sign-in, another state, a stray path are refused.
    #[test]
    fn the_redirect_gives_its_code_only_for_this_sign_in() {
        assert_eq!(code_from_request("GET /freesound/callback?code=XYZ%2F1&state=s1 HTTP/1.1\r\n", "s1"), Ok("XYZ/1".into()));
        assert!(code_from_request("GET /freesound/callback?code=XYZ&state=other HTTP/1.1", "s1").unwrap_err().contains("not for this"));
        assert!(code_from_request("GET /freesound/callback?error=access_denied&state=s1 HTTP/1.1", "s1").unwrap_err().contains("not approved"));
        assert!(code_from_request("GET /favicon.ico HTTP/1.1", "s1").unwrap_err().starts_with("not the callback"));
        assert!(code_from_request("GET /freesound/callback?state=s1 HTTP/1.1", "s1").is_err());
        assert_ne!(new_state(), new_state());
    }

    /// The tokens are kept readable by the user alone and read back; the
    /// access token counts as lapsed a minute early.
    #[test]
    fn the_sign_in_is_kept_for_the_user_alone() {
        let dir = std::env::temp_dir().join(format!("klipp-fs-oauth-{}", std::process::id()));
        let path = dir.join("freesound-oauth.json");
        save_at(&path, &tokens()).unwrap();
        assert_eq!(load_at(&path), Some(tokens()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(!tokens().lapsed(900) && tokens().lapsed(950) && tokens().lapsed(2_000));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The listener takes the code a browser brings back, answers it, and
    /// passes over a stray request first.
    #[test]
    fn the_listener_takes_the_redirect() {
        let state = "s-listen";
        let waiter = std::thread::spawn(move || await_redirect(state, std::time::Duration::from_secs(10)));
        let send = |line: &str| {
            for _ in 0..50 {
                if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", REDIRECT_PORT)) {
                    let _ = write!(s, "{line}\r\nHost: 127.0.0.1\r\n\r\n");
                    let mut answer = String::new();
                    let _ = std::io::Read::read_to_string(&mut s, &mut answer);
                    return answer;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            String::new()
        };
        let stray = send("GET /favicon.ico HTTP/1.1");
        if stray.is_empty() {
            eprintln!("UNVERIFIED: the sign-in port {REDIRECT_PORT} could not be reached here");
            return;
        }
        let answer = send("GET /freesound/callback?code=C0DE&state=s-listen HTTP/1.1");
        assert!(answer.contains("Signed in to Freesound"), "{answer}");
        assert_eq!(waiter.join().unwrap(), Ok("C0DE".into()));
    }
}
