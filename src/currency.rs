//! Currency conversion in the command bar: "100 usd to eur", "€50 in gbp", "20 £".
//!
//! Off unless `currency = on` in the settings file. Rates are the European
//! Central Bank's reference rates through frankfurter.dev (no key, no account);
//! nothing about the user is sent. They are fetched at most once a day, when
//! the command bar opens, and kept in `%APPDATA%\Islet\rates.txt`.

use crate::json::{self, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Mutex;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const WM_APP_RATES: u32 = WM_APP + 22;

const URL: &str = "https://api.frankfurter.dev/v1/latest";

/// The currencies the ECB publishes (and the only codes the parser accepts).
/// BGN left the list when Bulgaria joined the euro in 2026.
const CODES: &[&str] = &[
    "AUD", "BRL", "CAD", "CHF", "CNY", "CZK", "DKK", "EUR", "GBP", "HKD", "HUF", "IDR", "ILS", "INR", "ISK", "JPY", "KRW", "MXN", "MYR", "NOK", "NZD", "PHP", "PLN", "RON", "SEK", "SGD", "THB", "TRY", "USD", "ZAR",
];

/// Symbols and words people type for a currency (English and Lithuanian).
const NAMES: &[(&str, &str)] = &[
    ("$", "USD"), ("€", "EUR"), ("£", "GBP"), ("¥", "JPY"), ("zł", "PLN"), ("₹", "INR"), ("₩", "KRW"), ("₺", "TRY"), ("₪", "ILS"),
    ("dollar", "USD"), ("dollars", "USD"), ("euro", "EUR"), ("euros", "EUR"), ("pound", "GBP"), ("pounds", "GBP"), ("yen", "JPY"), ("franc", "CHF"), ("francs", "CHF"),
    ("zloty", "PLN"), ("zlotys", "PLN"), ("yuan", "CNY"), ("rupee", "INR"), ("rupees", "INR"),
    ("euras", "EUR"), ("eurai", "EUR"), ("eurų", "EUR"), ("eur.", "EUR"), ("doleris", "USD"), ("doleriai", "USD"), ("dolerių", "USD"),
    ("svaras", "GBP"), ("svarai", "GBP"), ("svarų", "GBP"), ("zlotų", "PLN"),
    ("eurus", "EUR"), ("dolerius", "USD"), ("svarus", "GBP"), ("zlotus", "PLN"), ("jenų", "JPY"), ("jenas", "JPY"),
];

/// Words that join the two currencies ("to", "in", Lithuanian "į").
const JOINERS: [&str; 8] = ["to", "in", "into", "as", "=", "->", "i", "į"];

#[derive(Clone, PartialEq, Debug)]
pub struct Rates {
    /// The ECB's date for these rates.
    pub date: String,
    /// The local day they were fetched (refreshed when it is not today).
    pub fetched: String,
    /// Units per euro.
    per_euro: Vec<(String, f64)>,
}

impl Rates {
    fn rate(&self, code: &str) -> Option<f64> {
        if code == "EUR" {
            return Some(1.0);
        }
        self.per_euro.iter().find(|(c, _)| c == code).map(|(_, r)| *r)
    }

    pub fn convert(&self, amount: f64, from: &str, to: &str) -> Option<f64> {
        Some(amount / self.rate(from)? * self.rate(to)?)
    }

    pub fn fresh(&self) -> bool {
        self.fetched == today()
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Query {
    pub amount: f64,
    pub from: &'static str,
    pub to: Option<&'static str>,
}

fn code_of(word: &str) -> Option<&'static str> {
    let upper = word.to_uppercase();
    CODES.iter().copied().find(|c| *c == upper).or_else(|| NAMES.iter().find(|(name, _)| *name == word).map(|(_, code)| *code))
}

/// "1,5" and "1.5" are one and a half; "1,000" and "1 000.50" have thousands.
/// A trailing "k" or "m" multiplies.
fn amount_of(word: &str) -> Option<f64> {
    let (digits, factor) = match word.strip_suffix('k').or_else(|| word.strip_suffix('m')) {
        Some(rest) => (rest, if word.ends_with('k') { 1e3 } else { 1e6 }),
        None => (word, 1.0),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit() || c == '.' || c == ',') || !digits.starts_with(|c: char| c.is_ascii_digit() || c == '.' || c == ',') {
        return None;
    }
    let normal = match (digits.rfind('.'), digits.rfind(',')) {
        // Both: the later one is the decimal mark.
        (Some(dot), Some(comma)) if dot > comma => digits.replace(',', ""),
        (Some(_), Some(_)) => digits.replace('.', "").replace(',', "."),
        // Only commas: "1,000" is a thousand, "1,5" one and a half.
        (None, Some(comma)) if digits.matches(',').count() > 1 || (digits.len() - comma - 1 == 3 && comma <= 3) => digits.replace(',', ""),
        (None, Some(_)) => digits.replace(',', "."),
        _ => digits.to_string(),
    };
    normal.parse::<f64>().ok().filter(|v| v.is_finite()).map(|v| v * factor)
}

/// Splits "€50" / "100usd" / "50$" into separate words.
fn words(text: &str) -> Vec<String> {
    let mut spaced = String::new();
    let mut previous: Option<char> = None;
    for c in text.to_lowercase().chars() {
        let symbol = matches!(c, '$' | '€' | '£' | '¥' | '₹' | '₩' | '₺' | '₪');
        let boundary = match previous {
            Some(p) => symbol || matches!(p, '$' | '€' | '£' | '¥' | '₹' | '₩' | '₺' | '₪') || (p.is_ascii_digit() && c.is_alphabetic() && c != 'k' && c != 'm'),
            None => false,
        };
        if boundary {
            spaced.push(' ');
        }
        spaced.push(c);
        previous = Some(c);
    }
    spaced.split_whitespace().map(str::to_string).collect()
}

/// Reads a conversion phrase; `None` for anything else.
pub fn parse(text: &str) -> Option<Query> {
    let (mut amount, mut codes) = (None, Vec::new());
    for word in words(text) {
        if JOINERS.contains(&word.as_str()) {
            continue;
        }
        if let Some(code) = code_of(&word) {
            codes.push(code);
        } else if let (None, Some(value)) = (amount, amount_of(&word)) {
            amount = Some(value);
        } else {
            return None;
        }
    }
    match codes.as_slice() {
        [from] => Some(Query { amount: amount?, from, to: None }),
        [from, to] => Some(Query { amount: amount?, from, to: Some(to) }),
        _ => None,
    }
}

/// "1 234.50" for showing, with two decimals (four below one).
pub fn show(value: f64) -> String {
    let text = copy_text(value);
    let (whole, rest) = text.split_once('.').map_or((text.as_str(), ""), |(w, r)| (w, r));
    let (sign, digits) = whole.strip_prefix('-').map_or(("", whole), |d| ("-", d));
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(' ');
        }
        grouped.push(c);
    }
    if rest.is_empty() { format!("{sign}{grouped}") } else { format!("{sign}{grouped}.{rest}") }
}

/// "1234.50" for the clipboard.
pub fn copy_text(value: f64) -> String {
    if value != 0.0 && value.abs() < 1.0 { format!("{value:.4}") } else { format!("{value:.2}") }
}

/// What the command bar shows for a phrase: (title, detail, text to copy).
pub fn rows(query: &Query, rates: Option<&Rates>, home: &str) -> Vec<(String, String, String)> {
    let Some(rates) = rates else {
        return vec![(format!("{} {} \u{2192} \u{2026}", show(query.amount), query.from), "Fetching exchange rates\u{2026}".into(), String::new())];
    };
    let targets: Vec<&str> = match query.to {
        Some(to) => vec![to],
        None => [home, "USD", "EUR", "GBP"].into_iter().filter(|c| *c != query.from).fold(Vec::new(), |mut list, c| {
            if !list.contains(&c) && list.len() < 2 {
                list.push(c);
            }
            list
        }),
    };
    let detail = format!("ECB {} \u{00B7} Enter copies", rates.date);
    targets
        .into_iter()
        .filter_map(|to| {
            let value = rates.convert(query.amount, query.from, to)?;
            Some((format!("{} {} = {} {to}", show(query.amount), query.from, show(value)), detail.clone(), copy_text(value)))
        })
        .collect()
}

// ------------------------------------------------------------------ storage --

fn today() -> String {
    let now = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}-{:02}-{:02}", now.wYear, now.wMonth, now.wDay)
}

fn file() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Islet").join("rates.txt"))
}

fn encode(rates: &Rates) -> String {
    let mut text = format!("{} {}\n", rates.fetched, rates.date);
    for (code, rate) in &rates.per_euro {
        text.push_str(&format!("{code} {rate}\n"));
    }
    text
}

fn decode(text: &str) -> Option<Rates> {
    let mut lines = text.lines();
    let (fetched, date) = lines.next()?.split_once(' ')?;
    let per_euro: Vec<(String, f64)> = lines.filter_map(|line| line.split_once(' ')).filter_map(|(code, rate)| Some((code.to_string(), rate.parse().ok()?))).collect();
    (!per_euro.is_empty()).then(|| Rates { date: date.to_string(), fetched: fetched.to_string(), per_euro })
}

/// The rates saved on disk, however old.
pub fn load() -> Option<Rates> {
    decode(&std::fs::read_to_string(file()?).ok()?)
}

/// Reads frankfurter.dev's answer (`{"base":"EUR","date":..,"rates":{"USD":1.08,..}}`).
fn parse_answer(body: &str) -> Option<Rates> {
    let root = json::parse(body)?;
    let Some(Value::Obj(list)) = root.get("rates") else { return None };
    let per_euro: Vec<(String, f64)> = list
        .iter()
        .filter_map(|(code, value)| match value {
            Value::Num(n) => Some((code.clone(), n.parse().ok()?)),
            _ => None,
        })
        .filter(|(_, rate)| *rate > 0.0)
        .collect();
    (root.text("base") == "EUR" && !per_euro.is_empty()).then(|| Rates { date: root.text("date").to_string(), fetched: today(), per_euro })
}

static LATEST: Mutex<Option<Rates>> = Mutex::new(None);
static FETCHING: AtomicBool = AtomicBool::new(false);

pub fn take() -> Option<Rates> {
    LATEST.lock().ok()?.take()
}

/// Fetches today's rates on a worker thread (once at a time); failures keep the old ones.
pub fn refresh(target: HWND) {
    if FETCHING.swap(true, Relaxed) {
        return;
    }
    let target = target.0 as isize;
    let _ = std::thread::Builder::new().name("islet-rates".into()).stack_size(256 * 1024).spawn(move || {
        if let Some(rates) = crate::net::get(URL).ok().and_then(|body| parse_answer(&body)) {
            if let Some(path) = file() {
                let _ = std::fs::write(path, encode(&rates));
            }
            if let Ok(mut latest) = LATEST.lock() {
                *latest = Some(rates);
            }
            unsafe {
                let _ = PostMessageW(Some(HWND(target as *mut _)), WM_APP_RATES, WPARAM(0), LPARAM(0));
            }
        }
        FETCHING.store(false, Relaxed);
    });
}

/// Made-up rates for tests and screenshots.
pub fn sample() -> Rates {
    Rates { date: "2026-11-13".into(), fetched: today(), per_euro: vec![("USD".into(), 1.08), ("GBP".into(), 0.85), ("JPY".into(), 162.5), ("PLN".into(), 4.3)] }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(amount: f64, from: &'static str, to: Option<&'static str>) -> Option<Query> {
        Some(Query { amount, from, to })
    }

    #[test]
    fn phrases() {
        assert_eq!(parse("100 usd to eur"), q(100.0, "USD", Some("EUR")));
        assert_eq!(parse("100USD in EUR"), q(100.0, "USD", Some("EUR")));
        assert_eq!(parse("€50 in gbp"), q(50.0, "EUR", Some("GBP")));
        assert_eq!(parse("50€ £"), q(50.0, "EUR", Some("GBP")));
        assert_eq!(parse("$1,000 to yen"), q(1000.0, "USD", Some("JPY")));
        assert_eq!(parse("1,5k pounds"), q(1500.0, "GBP", None));
        assert_eq!(parse("20 eurų į dolerius"), q(20.0, "EUR", Some("USD")));
        assert_eq!(parse("20 eurų į dolerių"), q(20.0, "EUR", Some("USD")));
        assert_eq!(parse("1 234,50 pln"), None);
        assert_eq!(parse("1.234,50 pln -> eur"), q(1234.5, "PLN", Some("EUR")));
        // Not conversions.
        for text in ["usd", "100", "timer 10 min", "100 abc to eur", "2+2", "100 usd eur gbp", "notepad"] {
            assert_eq!(parse(text), None, "{text}");
        }
    }

    #[test]
    fn converting_and_showing() {
        let rates = sample();
        assert!((rates.convert(108.0, "USD", "EUR").unwrap() - 100.0).abs() < 1e-9);
        assert!((rates.convert(100.0, "USD", "GBP").unwrap() - 78.7037).abs() < 1e-3);
        assert_eq!(rates.convert(1.0, "USD", "SEK"), None);
        assert_eq!((show(1234567.891), show(0.12346), show(-1500.0), show(12.0)), ("1 234 567.89".into(), "0.1235".into(), "-1 500.00".into(), "12.00".into()));
        let rows = rows(&parse("100 usd to eur").unwrap(), Some(&rates), "EUR");
        assert_eq!(rows, [("100.00 USD = 92.59 EUR".into(), "ECB 2026-11-13 \u{00B7} Enter copies".into(), "92.59".into())]);
        // No target: the home currency and the next most common one.
        let titles: Vec<String> = super::rows(&parse("10 gbp").unwrap(), Some(&rates), "EUR").into_iter().map(|r| r.0).collect();
        assert_eq!(titles, ["10.00 GBP = 11.76 EUR", "10.00 GBP = 12.71 USD"]);
        assert!(super::rows(&parse("10 gbp").unwrap(), None, "EUR")[0].1.starts_with("Fetching"));
    }

    #[test]
    fn answer_and_cache() {
        let rates = parse_answer(r#"{"amount":1.0,"base":"EUR","date":"2026-11-13","rates":{"USD":1.0812,"JPY":162.4,"BAD":"x"}}"#).unwrap();
        assert_eq!((rates.date.as_str(), rates.rate("USD"), rates.rate("JPY"), rates.rate("EUR"), rates.rate("BAD")), ("2026-11-13", Some(1.0812), Some(162.4), Some(1.0), None));
        assert!(rates.fresh());
        assert_eq!(decode(&encode(&rates)), Some(rates));
        assert_eq!(parse_answer(r#"{"base":"USD","rates":{"EUR":0.9}}"#), None);
        assert_eq!(decode("garbage"), None);
    }

    /// Asks frankfurter.dev for today's rates.
    #[test]
    #[ignore = "uses the network"]
    fn live_rates() {
        let body = crate::net::get(URL).unwrap();
        let rates = parse_answer(&body).unwrap();
        println!("ECB {}: 100 USD = {:.2} EUR, {} currencies", rates.date, rates.convert(100.0, "USD", "EUR").unwrap(), rates.per_euro.len());
        for code in CODES.iter().filter(|c| **c != "EUR") {
            assert!(rates.rate(code).is_some(), "{code} missing");
        }
    }
}
