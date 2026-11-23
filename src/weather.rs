//! Weather from Open-Meteo (no key): current conditions, 24 hours, 7 days.
//!
//! Off unless `weather = on` and a `city` are set in the settings file. The
//! only thing sent is the city name (once, to find its coordinates) and then
//! those coordinates. A worker thread fetches; `WM_APP_WEATHER` tells the UI.

use crate::json::{self, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const WM_APP_WEATHER: u32 = WM_APP + 18;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Sky {
    Clear,
    Partly,
    Cloudy,
    Fog,
    Rain,
    Snow,
    Storm,
}

impl Sky {
    /// From a WMO weather code.
    pub fn of(code: u32) -> Sky {
        match code {
            0 => Sky::Clear,
            1 | 2 => Sky::Partly,
            3 => Sky::Cloudy,
            45 | 48 => Sky::Fog,
            51..=67 | 80..=82 => Sky::Rain,
            71..=77 | 85 | 86 => Sky::Snow,
            95..=99 => Sky::Storm,
            _ => Sky::Cloudy,
        }
    }

    pub fn wet(self) -> bool {
        matches!(self, Sky::Rain | Sky::Snow | Sky::Storm)
    }
}

/// A short description of a WMO weather code.
pub fn describe(code: u32) -> &'static str {
    match code {
        0 => "Clear sky",
        1 => "Mostly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51..=55 => "Drizzle",
        56 | 57 => "Freezing drizzle",
        61 => "Light rain",
        63 => "Rain",
        65 => "Heavy rain",
        66 | 67 => "Freezing rain",
        71 => "Light snow",
        73 => "Snow",
        75 => "Heavy snow",
        77 => "Snow grains",
        80..=82 => "Showers",
        85 | 86 => "Snow showers",
        95 => "Thunderstorm",
        96 | 99 => "Thunderstorm with hail",
        _ => "Cloudy",
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Hour {
    pub hour: u8,
    pub temp: f32,
    pub code: u32,
    pub sky: Sky,
    pub day: bool,
    /// Chance of precipitation in percent, and the amount in mm.
    pub chance: u8,
    pub mm: f32,
    /// km/h and percent.
    pub wind: f32,
    pub humidity: f32,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Day {
    /// 0 = Monday.
    pub weekday: u8,
    pub code: u32,
    pub sky: Sky,
    pub max: f32,
    pub min: f32,
    /// Highest chance of precipitation (percent) and the day's total (mm).
    pub chance: u8,
    pub mm: f32,
    pub uv: f32,
    pub wind: f32,
    /// Minutes after local midnight (None near the poles).
    pub sunrise: Option<u16>,
    pub sunset: Option<u16>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Weather {
    pub place: String,
    pub temp: f32,
    pub feels: f32,
    pub code: u32,
    pub sky: Sky,
    pub day: bool,
    pub wind: f32,
    /// Where the wind comes from, degrees (0 = north), and gusts in km/h.
    pub wind_dir: f32,
    pub gusts: f32,
    pub humidity: f32,
    pub dew: f32,
    /// Sea-level pressure in hPa and its change over the next three hours.
    pub pressure: f32,
    pub pressure_trend: f32,
    pub clouds: f32,
    pub uv: f32,
    /// Metres.
    pub visibility: f32,
    /// European air quality index and PM2.5 (µg/m³), when that answer came.
    pub aqi: Option<f32>,
    pub pm25: Option<f32>,
    /// Local time of the reading, minutes after midnight.
    pub minute: u16,
    /// From the current hour on.
    pub hours: Vec<Hour>,
    pub days: Vec<Day>,
    /// Dry now, precipitation expected in the coming hour.
    pub rain_soon: bool,
}

impl Weather {
    /// 0 at night, 1 by day, with dawn and dusk blended over the hour around
    /// sunrise and sunset (for the sky's colours).
    pub fn daylight(&self, minute: f32) -> f32 {
        let Some(today) = self.days.first() else { return if self.day { 1.0 } else { 0.0 } };
        let (Some(rise), Some(set)) = (today.sunrise, today.sunset) else { return if self.day { 1.0 } else { 0.0 } };
        let ramp = |m: f32, edge: f32| ((m - edge) / 60.0 + 0.5).clamp(0.0, 1.0);
        ramp(minute, rise as f32).min(1.0 - ramp(minute, set as f32))
    }

    /// How warm the light is: 1 right at sunrise or sunset, 0 an hour or more away.
    pub fn golden(&self, minute: f32) -> f32 {
        let Some(today) = self.days.first() else { return 0.0 };
        [today.sunrise, today.sunset].iter().flatten().map(|&edge| (1.0 - (minute - edge as f32).abs() / 60.0).max(0.0)).fold(0.0, f32::max)
    }
}

/// "07:34" from the ISO time "2026-11-16T07:34".
fn minute_of(time: &str) -> Option<u16> {
    let h: u16 = time.get(11..13)?.parse().ok()?;
    let m: u16 = time.get(14..16)?.parse().ok()?;
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// Name of the European air quality index band.
pub fn air_band(aqi: f32) -> &'static str {
    match aqi as u32 {
        0..=20 => "Good",
        21..=40 => "Fair",
        41..=60 => "Moderate",
        61..=80 => "Poor",
        81..=100 => "Very poor",
        _ => "Extremely poor",
    }
}

/// Name of the UV index band.
pub fn uv_band(uv: f32) -> &'static str {
    match uv {
        u if u < 3.0 => "Low",
        u if u < 6.0 => "Moderate",
        u if u < 8.0 => "High",
        u if u < 11.0 => "Very high",
        _ => "Extreme",
    }
}

/// "N", "NE" ... for where the wind comes from.
pub fn compass(degrees: f32) -> &'static str {
    const NAMES: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    NAMES[((degrees.rem_euclid(360.0) + 22.5) / 45.0) as usize % 8]
}

fn number(value: Option<&Value>) -> Option<f32> {
    match value? {
        Value::Num(text) => text.parse().ok(),
        _ => None,
    }
}

fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    match value.get(key) {
        Some(Value::Arr(items)) => items,
        _ => &[],
    }
}

/// Monday = 0 for a "YYYY-MM-DD" date (Sakamoto's method).
fn weekday(date: &str) -> Option<u8> {
    let mut parts = date.get(..10)?.split('-').map(|p| p.parse::<i32>().ok());
    let (mut y, m, d) = (parts.next()??, parts.next()??, parts.next()??);
    if !(1..=12).contains(&m) {
        return None;
    }
    if m < 3 {
        y -= 1;
    }
    let sunday_based = (y + y / 4 - y / 100 + y / 400 + [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4][(m - 1) as usize] + d).rem_euclid(7);
    Some(((sunday_based + 6) % 7) as u8)
}

/// Reads an Open-Meteo forecast answer.
pub fn parse(place: &str, body: &str) -> Option<Weather> {
    let root = json::parse(body)?;
    let current = root.get("current")?;
    let code = number(current.get("weather_code"))? as u32;
    let now = current.text("time");
    let hourly = root.get("hourly")?;
    let times = list(hourly, "time");
    // The entry of the running hour: "2026-11-14T15:15" -> "2026-11-14T15".
    let first = times.iter().position(|t| t.str().is_some_and(|t| now.len() >= 13 && t.starts_with(&now[..13]))).unwrap_or(0);
    let column = |key: &str, i: usize| number(list(hourly, key).get(i));
    let mut hours = Vec::new();
    for (i, time) in times.iter().enumerate().skip(first).take(24) {
        let hour = time.str().and_then(|t| t.get(11..13)).and_then(|h| h.parse().ok()).unwrap_or(0);
        let code = column("weather_code", i)? as u32;
        hours.push(Hour {
            hour,
            temp: column("temperature_2m", i)?,
            code,
            sky: Sky::of(code),
            day: column("is_day", i).unwrap_or(1.0) > 0.5,
            chance: column("precipitation_probability", i).unwrap_or(0.0) as u8,
            mm: column("precipitation", i).unwrap_or(0.0),
            wind: column("wind_speed_10m", i).unwrap_or(0.0),
            humidity: column("relative_humidity_2m", i).unwrap_or(0.0),
        });
    }
    let daily = root.get("daily")?;
    let mut days = Vec::new();
    let field = |key: &str, i: usize| number(list(daily, key).get(i));
    let clock = |key: &str, i: usize| list(daily, key).get(i).and_then(|t| t.str()).and_then(minute_of);
    for (i, date) in list(daily, "time").iter().enumerate().take(7) {
        let code = field("weather_code", i)? as u32;
        days.push(Day {
            weekday: weekday(date.str()?)?,
            code,
            sky: Sky::of(code),
            max: field("temperature_2m_max", i)?,
            min: field("temperature_2m_min", i)?,
            chance: field("precipitation_probability_max", i).unwrap_or(0.0) as u8,
            mm: field("precipitation_sum", i).unwrap_or(0.0),
            uv: field("uv_index_max", i).unwrap_or(0.0),
            wind: field("wind_speed_10m_max", i).unwrap_or(0.0),
            sunrise: clock("sunrise", i),
            sunset: clock("sunset", i),
        });
    }
    let sky = Sky::of(code);
    let dry_now = !sky.wet() && number(current.get("precipitation")).unwrap_or(0.0) < 0.05;
    let rain_soon = dry_now && hours.get(1).is_some_and(|next| next.sky.wet() && (next.chance >= 60 || next.mm >= 0.2));
    let pressure = number(current.get("pressure_msl")).unwrap_or(0.0);
    // Three hours ahead, or as far as the list goes.
    let later = (1..=3).rev().find_map(|k| column("pressure_msl", first + k)).unwrap_or(pressure);
    Some(Weather {
        place: place.to_string(),
        temp: number(current.get("temperature_2m"))?,
        feels: number(current.get("apparent_temperature")).unwrap_or(0.0),
        code,
        sky,
        day: number(current.get("is_day")).unwrap_or(1.0) > 0.5,
        wind: number(current.get("wind_speed_10m")).unwrap_or(0.0),
        wind_dir: number(current.get("wind_direction_10m")).unwrap_or(0.0),
        gusts: number(current.get("wind_gusts_10m")).unwrap_or(0.0),
        humidity: number(current.get("relative_humidity_2m")).unwrap_or(0.0),
        dew: number(current.get("dew_point_2m")).unwrap_or(0.0),
        pressure,
        pressure_trend: if pressure > 0.0 { later - pressure } else { 0.0 },
        clouds: number(current.get("cloud_cover")).unwrap_or(0.0),
        uv: number(current.get("uv_index")).unwrap_or(0.0),
        visibility: number(current.get("visibility")).unwrap_or(0.0),
        aqi: None,
        pm25: None,
        minute: minute_of(now).unwrap_or(12 * 60),
        hours,
        days,
        rain_soon,
    })
}

/// Reads an Open-Meteo air quality answer: (European AQI, PM2.5).
pub fn parse_air(body: &str) -> Option<(f32, Option<f32>)> {
    let root = json::parse(body)?;
    let current = root.get("current")?;
    Some((number(current.get("european_aqi"))?, number(current.get("pm2_5"))))
}

/// A made-up forecast for tests and screenshots.
pub fn sample() -> Weather {
    let skies = [(2, Sky::Partly), (2, Sky::Partly), (3, Sky::Cloudy), (63, Sky::Rain), (61, Sky::Rain), (3, Sky::Cloudy), (0, Sky::Clear), (0, Sky::Clear)];
    let hours = (0..24)
        .map(|i| {
            let (code, sky) = skies[i / 3];
            let wave = (i as f32 * 0.45).sin();
            Hour {
                hour: ((15 + i) % 24) as u8,
                temp: 9.0 - (i as f32 * 0.35) + wave * 1.2,
                code,
                sky,
                day: (15 + i) % 24 >= 8 && (15 + i) % 24 < 17,
                chance: if sky == Sky::Rain { 70 + (i as u8 % 3) * 8 } else { (i as u8 * 3) % 20 },
                mm: if sky == Sky::Rain { 0.6 + (i % 3) as f32 * 0.5 } else { 0.0 },
                wind: 14.0 + wave * 6.0,
                humidity: 70.0 + wave * 12.0,
            }
        })
        .collect();
    let week = [(2, Sky::Partly), (63, Sky::Rain), (61, Sky::Rain), (3, Sky::Cloudy), (73, Sky::Snow), (0, Sky::Clear), (95, Sky::Storm)];
    let days = (0..7)
        .map(|i| {
            let (code, sky) = week[i];
            Day {
                weekday: ((5 + i) % 7) as u8,
                code,
                sky,
                max: 9.0 - i as f32 + if i == 5 { 3.0 } else { 0.0 },
                min: 3.0 - i as f32 * 1.3,
                chance: if sky.wet() { 60 + i as u8 * 5 } else { 10 },
                mm: if sky.wet() { 2.0 + i as f32 } else { 0.0 },
                uv: 1.5 + (i % 3) as f32,
                wind: 18.0 + i as f32 * 3.0,
                sunrise: Some(7 * 60 + 34 + i as u16 * 2),
                sunset: Some(16 * 60 + 12 - i as u16 * 2),
            }
        })
        .collect();
    Weather {
        place: "Vilnius".into(),
        temp: 8.6,
        feels: 6.1,
        code: 2,
        sky: Sky::Partly,
        day: true,
        wind: 14.0,
        wind_dir: 238.0,
        gusts: 31.0,
        humidity: 71.0,
        dew: 3.6,
        pressure: 1012.4,
        pressure_trend: -1.6,
        clouds: 45.0,
        uv: 1.2,
        visibility: 24000.0,
        aqi: Some(23.0),
        pm25: Some(6.9),
        minute: 15 * 60 + 15,
        hours,
        days,
        rain_soon: false,
    }
}

static LATEST: Mutex<Option<Result<Weather, String>>> = Mutex::new(None);

pub fn take() -> Option<Result<Weather, String>> {
    LATEST.lock().ok()?.take()
}

/// `%APPDATA%\Islet`.
fn folder() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Islet"))
}

/// Coordinates and display name of `city`, remembered so the name is sent once.
fn locate(city: &str, dir: &Path) -> Result<(String, String, String), String> {
    let file = dir.join("weather-place.txt");
    if let Ok(saved) = std::fs::read_to_string(&file) {
        let parts: Vec<&str> = saved.trim().split('|').collect();
        if parts.len() == 4 && parts[0] == city {
            return Ok((parts[1].to_string(), parts[2].to_string(), parts[3].to_string()));
        }
    }
    let body = crate::net::get(&format!("https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=en&format=json", crate::net::escape(city)))?;
    let root = json::parse(&body).ok_or("Unreadable answer from the place search")?;
    let place = list(&root, "results").first().ok_or_else(|| format!("No place called \u{201C}{city}\u{201D} was found"))?;
    let coordinate = |key: &str| match place.get(key) {
        Some(Value::Num(text)) => Ok(text.clone()),
        _ => Err("The place search gave no coordinates".to_string()),
    };
    let found = (coordinate("latitude")?, coordinate("longitude")?, place.text("name").to_string());
    let _ = std::fs::write(&file, format!("{city}|{}|{}|{}", found.0, found.1, found.2));
    Ok(found)
}

fn fetch(city: &str) -> Result<Weather, String> {
    let dir = folder().ok_or("No settings folder")?;
    let _ = std::fs::create_dir_all(&dir);
    let (latitude, longitude, name) = locate(city, &dir)?;
    let body = crate::net::get(&forecast_url(&latitude, &longitude))?;
    let mut weather = parse(&name, &body).ok_or_else(|| "Unreadable forecast".to_string())?;
    // Air quality is a second, optional answer from the same service.
    let air = format!("https://air-quality-api.open-meteo.com/v1/air-quality?latitude={latitude}&longitude={longitude}&current=european_aqi,pm2_5&timezone=auto");
    if let Some((aqi, pm25)) = crate::net::get(&air).ok().and_then(|body| parse_air(&body)) {
        (weather.aqi, weather.pm25) = (Some(aqi), pm25);
    }
    Ok(weather)
}

fn forecast_url(latitude: &str, longitude: &str) -> String {
    format!(
        "https://api.open-meteo.com/v1/forecast?latitude={latitude}&longitude={longitude}\
         &current=temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,is_day,wind_speed_10m,wind_direction_10m,\
         wind_gusts_10m,precipitation,pressure_msl,cloud_cover,dew_point_2m,uv_index,visibility\
         &hourly=temperature_2m,weather_code,is_day,precipitation_probability,precipitation,wind_speed_10m,relative_humidity_2m,pressure_msl\
         &daily=weather_code,temperature_2m_max,temperature_2m_min,sunrise,sunset,uv_index_max,precipitation_probability_max,\
         precipitation_sum,wind_speed_10m_max&timezone=auto&forecast_days=7"
    )
}

/// Fetches the forecast for `city` on a worker thread.
pub fn refresh(target: HWND, city: String) {
    let target = target.0 as isize;
    let _ = std::thread::Builder::new().name("islet-weather".into()).stack_size(512 * 1024).spawn(move || {
        let result = fetch(&city);
        if let Ok(mut latest) = LATEST.lock() {
            *latest = Some(result);
        }
        unsafe {
            let _ = PostMessageW(Some(HWND(target as *mut _)), WM_APP_WEATHER, WPARAM(0), LPARAM(0));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWER: &str = r#"{"latitude":54.6875,"timezone":"Europe/Vilnius",
      "current":{"time":"2026-11-14T15:15","temperature_2m":8.6,"apparent_temperature":6.1,"relative_humidity_2m":71,"weather_code":2,"is_day":1,"wind_speed_10m":14.2,"precipitation":0.0,
        "wind_direction_10m":238,"wind_gusts_10m":40.7,"pressure_msl":999.9,"cloud_cover":100,"dew_point_2m":5.7,"uv_index":0.4,"visibility":32480.00},
      "hourly":{"time":["2026-11-14T13:00","2026-11-14T14:00","2026-11-14T15:00","2026-11-14T16:00","2026-11-14T17:00"],
        "temperature_2m":[9.5,9.1,8.6,8.0,7.4],"weather_code":[1,2,2,61,63],"is_day":[1,1,1,1,0],
        "precipitation_probability":[0,5,10,75,90],"precipitation":[0.0,0.0,0.0,0.6,1.4],
        "wind_speed_10m":[10,11,14,18,20],"relative_humidity_2m":[60,65,71,80,88],"pressure_msl":[1001,1000.5,999.9,999,998]},
      "daily":{"time":["2026-11-14","2026-11-15"],"weather_code":[61,71],"temperature_2m_max":[9.6,4.0],"temperature_2m_min":[5.1,-1.5],
        "sunrise":["2026-11-14T07:34","2026-11-15T07:36"],"sunset":["2026-11-14T16:12","2026-11-15T16:10"],
        "uv_index_max":[1.1,0.8],"precipitation_probability_max":[90,40],"precipitation_sum":[4.2,1.0],"wind_speed_10m_max":[24,19]}}"#;

    #[test]
    fn reads_a_forecast() {
        let w = parse("Vilnius", ANSWER).unwrap();
        assert_eq!((w.temp, w.feels, w.code, w.sky, w.day), (8.6, 6.1, 2, Sky::Partly, true));
        assert_eq!((w.wind, w.humidity, w.wind_dir, w.gusts, w.dew, w.clouds, w.uv), (14.2, 71.0, 238.0, 40.7, 5.7, 100.0, 0.4));
        assert_eq!((w.pressure, w.visibility, w.minute, w.aqi), (999.9, 32480.0, 15 * 60 + 15, None));
        // Only two hours follow the running one, so the trend uses what is there (998 at 17:00 is index 4).
        assert!((w.pressure_trend - (998.0 - 999.9)).abs() < 0.01);
        assert_eq!((w.hours[1].wind, w.hours[1].humidity, w.hours[1].code), (18.0, 80.0, 61));
        let today = &w.days[0];
        assert_eq!((today.sunrise, today.sunset, today.chance, today.mm, today.uv, today.wind), (Some(454), Some(972), 90, 4.2, 1.1, 24.0));
        // Daylight: noon full, midnight none, half at sunrise; golden at sunset.
        assert_eq!((w.daylight(720.0), w.daylight(0.0), w.daylight(454.0)), (1.0, 0.0, 0.5));
        assert_eq!((w.golden(972.0), w.golden(720.0)), (1.0, 0.0));
        assert_eq!(parse_air(r#"{"current":{"time":"x","european_aqi":33,"pm2_5":6.9}}"#), Some((33.0, Some(6.9))));
        assert_eq!((compass(238.0), compass(359.0), compass(90.0)), ("SW", "N", "E"));
        assert_eq!((air_band(33.0), uv_band(6.5)), ("Fair", "High"));
        // Hours start at the running one (15:00), not at the start of the list.
        assert_eq!(w.hours.len(), 3);
        assert_eq!((w.hours[0].hour, w.hours[0].temp), (15, 8.6));
        assert_eq!((w.hours[1].sky, w.hours[1].chance, w.hours[2].day), (Sky::Rain, 75, false));
        // 14 November 2026 is a Saturday.
        assert_eq!((w.days[0].weekday, w.days[0].sky, w.days[1].weekday, w.days[1].min), (5, Sky::Rain, 6, -1.5));
        // Dry now, rain in the next hour.
        assert!(w.rain_soon);
        assert_eq!(parse("x", "{}"), None);
        assert_eq!(parse("x", "not json"), None);
    }

    #[test]
    fn codes_and_weekdays() {
        assert_eq!((Sky::of(0), Sky::of(48), Sky::of(81), Sky::of(86), Sky::of(96)), (Sky::Clear, Sky::Fog, Sky::Rain, Sky::Snow, Sky::Storm));
        assert_eq!(describe(65), "Heavy rain");
        assert_eq!((weekday("2024-02-29"), weekday("2026-01-01"), weekday("2026-13-01"), weekday("x")), (Some(3), Some(3), None, None));
    }

    /// Asks Open-Meteo for a real forecast (Berlin, by coordinates).
    #[test]
    #[ignore = "uses the network"]
    fn live_forecast() {
        let body = crate::net::get(&forecast_url("52.52", "13.41")).unwrap();
        let w = parse("Berlin", &body).unwrap();
        println!("{} C, {}, {} hours, {} days, {} hPa ({:+.1}), sun {:?}-{:?}", w.temp, describe(w.code), w.hours.len(), w.days.len(), w.pressure, w.pressure_trend, w.days[0].sunrise, w.days[0].sunset);
        assert_eq!((w.hours.len(), w.days.len()), (24, 7));
        assert!(w.pressure > 900.0 && w.days[0].sunrise.is_some());
        let air = crate::net::get("https://air-quality-api.open-meteo.com/v1/air-quality?latitude=52.52&longitude=13.41&current=european_aqi,pm2_5&timezone=auto").unwrap();
        assert!(parse_air(&air).is_some());
    }
}
