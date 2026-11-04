//! "Add display" form.
//!
//! Testing: defaults, field cycling and name validation are unit-tested.

use castdesk_core::{BackendKind, Codec, DisplaySpec, EncoderPref, Placement, VideoSettings};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Choice,
}

#[derive(Debug, Clone)]
pub struct Field {
    pub label: &'static str,
    pub kind: FieldKind,
    pub value: String,
    pub choices: Vec<String>,
    pub hint: &'static str,
}

impl Field {
    fn text(label: &'static str, value: impl Into<String>, hint: &'static str) -> Self {
        Self { label, kind: FieldKind::Text, value: value.into(), choices: vec![], hint }
    }
    fn choice(label: &'static str, choices: &[&str], hint: &'static str) -> Self {
        Self {
            label,
            kind: FieldKind::Choice,
            value: choices[0].to_string(),
            choices: choices.iter().map(|s| s.to_string()).collect(),
            hint,
        }
    }
    pub fn cycle(&mut self, step: i32) {
        if self.choices.is_empty() {
            return;
        }
        let i = self.choices.iter().position(|c| *c == self.value).unwrap_or(0) as i32;
        let n = self.choices.len() as i32;
        self.value = self.choices[((i + step).rem_euclid(n)) as usize].clone();
    }
}

#[derive(Debug, Clone)]
pub struct AddForm {
    pub fields: Vec<Field>,
    pub focus: usize,
    pub error: Option<String>,
}

pub const RESOLUTIONS: &[&str] = &["1920x1080", "1280x720", "2560x1440", "3840x2160", "1600x900"];

impl AddForm {
    pub fn new(suggested_name: String, defaults: &VideoSettings) -> Self {
        let mut res = Field::choice("Resolution", RESOLUTIONS, "←/→ to pick");
        res.value = format!("{}x{}", defaults.width, defaults.height);
        if !res.choices.contains(&res.value) {
            res.choices.insert(0, res.value.clone());
        }
        let backends: Vec<&str> = BackendKind::ALL.iter().map(|b| b.label()).collect();
        let placements: Vec<&str> = Placement::ALL.iter().map(|p| p.label()).collect();
        Self {
            fields: vec![
                Field::text("Name", suggested_name, "RTSP path; a-z 0-9 - _"),
                res,
                Field::text("FPS", defaults.fps.to_string(), "1-144"),
                Field::text("Bitrate Mb/s", format!("{}", defaults.bitrate_kbps as f64 / 1000.0), "8-15 recommended on gigabit"),
                Field::choice("Codec", &["h264", "h265"], "H.265 halves bitrate; check receiver support"),
                Field::choice("Encoder", &["auto", "vaapi", "nvenc", "software"], "auto = VA-API → NVENC → x264"),
                Field::choice("Backend", &backends, "auto = detect from session; test = color bars"),
                Field::choice("Placement", &placements, "relative to the laptop screen"),
            ],
            focus: 0,
            error: None,
        }
    }

    pub fn focused(&mut self) -> &mut Field {
        &mut self.fields[self.focus]
    }

    pub fn next(&mut self) {
        self.focus = (self.focus + 1) % self.fields.len();
    }

    pub fn prev(&mut self) {
        self.focus = (self.focus + self.fields.len() - 1) % self.fields.len();
    }

    fn get(&self, label: &str) -> &str {
        &self.fields.iter().find(|f| f.label == label).unwrap().value
    }

    pub fn to_spec(&self) -> Result<DisplaySpec, String> {
        let (w, h) = parse_resolution(self.get("Resolution"))?;
        let fps: u32 = self.get("FPS").trim().parse().map_err(|_| "FPS must be a number")?;
        let mbps: f64 = self
            .get("Bitrate Mb/s")
            .trim()
            .parse()
            .map_err(|_| "bitrate must be a number (Mb/s)")?;
        let codec = match self.get("Codec") {
            "h265" => Codec::H265,
            _ => Codec::H264,
        };
        let encoder = match self.get("Encoder") {
            "vaapi" => EncoderPref::Vaapi,
            "nvenc" => EncoderPref::Nvenc,
            "software" => EncoderPref::Software,
            _ => EncoderPref::Auto,
        };
        let backend = BackendKind::ALL
            .into_iter()
            .find(|b| b.label() == self.get("Backend"))
            .unwrap_or_default();
        let placement = Placement::ALL
            .into_iter()
            .find(|p| p.label() == self.get("Placement"))
            .unwrap_or_default();
        let spec = DisplaySpec {
            name: self.get("Name").trim().to_string(),
            backend,
            placement,
            video: VideoSettings {
                width: w,
                height: h,
                fps,
                bitrate_kbps: (mbps * 1000.0).round() as u32,
                codec,
                encoder,
                ..Default::default()
            },
            // Stage (b) stubs, not offered in the form yet.
            destinations: Vec::new(),
            audio: false,
            latency_overlay: false,
        };
        spec.validate()?;
        Ok(spec)
    }
}

pub fn parse_resolution(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s
        .trim()
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("resolution {s:?} should look like 1920x1080"))?;
    Ok((
        w.trim().parse().map_err(|_| "bad width")?,
        h.trim().parse().map_err(|_| "bad height")?,
    ))
}

/// First unused `tvN`.
pub fn suggest_name(existing: &[String]) -> String {
    (1..)
        .map(|i| format!("tv{i}"))
        .find(|n| !existing.contains(n))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_form_makes_valid_spec() {
        let f = AddForm::new("tv1".into(), &VideoSettings::default());
        let s = f.to_spec().unwrap();
        assert_eq!(s.name, "tv1");
        assert_eq!((s.video.width, s.video.height, s.video.fps), (1920, 1080, 60));
        assert_eq!(s.video.bitrate_kbps, 12_000);
    }

    #[test]
    fn cycling_and_errors() {
        let mut f = AddForm::new("tv1".into(), &VideoSettings::default());
        f.focus = 1;
        f.focused().cycle(1);
        assert_eq!(f.fields[1].value, "1280x720");
        f.focused().cycle(-2);
        assert_eq!(f.fields[1].value, "1600x900");
        f.fields[2].value = "abc".into();
        assert!(f.to_spec().is_err());
        f.fields[2].value = "30".into();
        f.fields[0].value = "Living Room".into();
        assert!(f.to_spec().unwrap_err().contains("invalid display name"));
    }

    #[test]
    fn names() {
        assert_eq!(suggest_name(&["tv1".into(), "tv3".into()]), "tv2");
        assert_eq!(parse_resolution("1280 x 720").unwrap(), (1280, 720));
    }
}
