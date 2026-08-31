//! Locale handling and statistical locale selection.
//!
//! The CLDR `territoryInfo.xml` data is embedded at compile time; the selector
//! picks languages/regions with weighted, population-based logic.

use std::collections::HashSet;
use std::sync::OnceLock;

use rand::Rng;
use serde_json::{Map, Value};

use crate::error::{CamoufoxError, Result};
use crate::mappings::warnings;

/// Embedded CLDR territory language data (from `src/data-files/territoryInfo.xml`).
const TERRITORY_INFO_XML: &str = include_str!("data/territoryInfo.xml");

/// A parsed language tag: language, optional script and region.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Locale {
    /// ISO language code, lowercase (e.g. `en`).
    pub language: String,
    /// ISO region code, uppercase (e.g. `US`).
    pub region: Option<String>,
    /// ISO script code, title case (e.g. `Latn`).
    pub script: Option<String>,
}

impl Locale {
    /// Builds a locale with only a language (used by `handle_locale` with
    /// `ignore_region`).
    pub fn language_only(language: impl Into<String>) -> Self {
        Self {
            language: language.into(),
            region: None,
            script: None,
        }
    }

    /// `language[-script][-region]` rendering.
    pub fn as_string(&self) -> String {
        let mut out = self.language.clone();
        if let Some(script) = &self.script {
            out.push('-');
            out.push_str(script);
        }
        if let Some(region) = &self.region {
            out.push('-');
            out.push_str(region);
        }
        out
    }

    /// The `locale:*` config entries (`Locale.asConfig`).
    pub fn as_config(&self) -> Result<Map<String, Value>> {
        let Some(region) = &self.region else {
            return Err(CamoufoxError::LocaleError("Region is required for config".into()));
        };
        let mut data = Map::new();
        data.insert("locale:region".into(), Value::String(region.clone()));
        data.insert(
            "locale:language".into(),
            Value::String(self.language.clone()),
        );
        if let Some(script) = &self.script {
            data.insert("locale:script".into(), Value::String(script.clone()));
        }
        Ok(data)
    }
}

/// A resolved geolocation for an IP.
#[derive(Debug, Clone)]
pub struct Geolocation {
    /// The statistically selected locale for the IP's region.
    pub locale: Locale,
    /// Longitude.
    pub longitude: f64,
    /// Latitude.
    pub latitude: f64,
    /// IANA timezone.
    pub timezone: String,
    /// Optional accuracy radius (km).
    pub accuracy: Option<u32>,
}

impl Geolocation {
    /// The `geolocation:*`, `timezone` and `locale:*` config entries
    /// (`Geolocation.asConfig`).
    pub fn as_config(&self) -> Result<Map<String, Value>> {
        let mut data = Map::new();
        if let Some(n) = serde_json::Number::from_f64(self.longitude) {
            data.insert("geolocation:longitude".into(), Value::Number(n));
        }
        if let Some(n) = serde_json::Number::from_f64(self.latitude) {
            data.insert("geolocation:latitude".into(), Value::Number(n));
        }
        data.insert("timezone".into(), Value::String(self.timezone.clone()));
        for (key, value) in self.locale.as_config()? {
            data.insert(key, value);
        }
        if let Some(accuracy) = self.accuracy {
            data.insert("geolocation:accuracy".into(), Value::from(accuracy));
        }
        Ok(data)
    }
}

// -- territoryInfo.xml ----------------------------------------------------------

#[derive(Debug, Clone)]
struct LanguagePopulation {
    language: String,
    population_percent: f64,
}

#[derive(Debug, Clone)]
struct Territory {
    code: String,
    literacy_percent: f64,
    population: f64,
    languages: Vec<LanguagePopulation>,
}/// Parsed CLDR territory data (territories with their language populations).
#[derive(Debug, Default)]
pub struct TerritoryInfo {
    territories: Vec<Territory>,
}

fn attr<'a>(e: &quick_xml::events::attributes::Attribute<'a>, name: &str) -> Option<String> {
    if e.key.as_ref() == name.as_bytes() {
        Some(String::from_utf8_lossy(&e.value).into_owned())
    } else {
        None
    }
}

fn parse_territory_info(xml: &str) -> Result<TerritoryInfo> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut info = TerritoryInfo::default();
    let mut current_territory: Option<Territory> = None;

    fn read_lang_pop(
        e: &quick_xml::events::BytesStart<'_>,
        current_territory: &mut Option<Territory>,
    ) {
        let mut language = String::new();
        let mut percent = 0.0;
        for a in e.attributes().flatten() {
            if let Some(v) = attr(&a, "type") {
                language = v;
            } else if let Some(v) = attr(&a, "populationPercent") {
                percent = v.parse().unwrap_or(0.0);
            }
        }
        if let Some(territory) = current_territory.as_mut() {
            territory.languages.push(LanguagePopulation {
                language,
                population_percent: percent,
            });
        }
    }

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name();
                if name.as_ref() == b"territory" {
                    let mut code = String::new();
                    let mut literacy = 0.0;
                    let mut population = 0.0;
                    for a in e.attributes().flatten() {
                        if let Some(v) = attr(&a, "type") {
                            code = v;
                        } else if let Some(v) = attr(&a, "literacyPercent") {
                            literacy = v.parse().unwrap_or(0.0);
                        } else if let Some(v) = attr(&a, "population") {
                            population = v.parse().unwrap_or(0.0);
                        }
                    }
                    current_territory = Some(Territory {
                        code,
                        literacy_percent: literacy,
                        population,
                        languages: Vec::new(),
                    });
                } else if name.as_ref() == b"languagePopulation" {
                    read_lang_pop(&e, &mut current_territory);
                }
            }
            // <languagePopulation .../> is a self-closing tag → Empty event.
            Ok(Event::Empty(e)) => {
                if e.name().as_ref() == b"languagePopulation" {
                    read_lang_pop(&e, &mut current_territory);
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == b"territory" {
                    if let Some(t) = current_territory.take() {
                        info.territories.push(t);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => {
                return Err(CamoufoxError::Xml(format!("territoryInfo parse error: {e}")));
            }
        }
        buf.clear();
    }
    Ok(info)
}

/// Parsed territory data (singleton).
pub fn territory_info() -> &'static TerritoryInfo {
    static INFO: OnceLock<TerritoryInfo> = OnceLock::new();
    INFO.get_or_init(|| parse_territory_info(TERRITORY_INFO_XML).expect("embedded territoryInfo.xml is valid"))
}

fn weighted_random_choice<T: Clone>(items: &[T], weights: &[f64]) -> Option<T> {
    if items.is_empty() || items.len() != weights.len() {
        return None;
    }
    let total: f64 = weights.iter().sum();
    if total <= 0.0 {
        // JS fallback: uniform choice.
        let idx = rand::thread_rng().gen_range(0..items.len());
        return Some(items[idx].clone());
    }
    let r: f64 = rand::thread_rng().gen::<f64>() * total;
    let mut acc = 0.0;
    for (i, w) in weights.iter().enumerate() {
        acc += w;
        if r < acc {
            return Some(items[i].clone());
        }
    }
    items.last().cloned()
}

/// `SELECTOR.fromRegion(region)`: picks a language for the territory weighted
/// by `populationPercent`, then normalizes `language-REGION`.
pub fn from_region(region: &str) -> Result<Locale> {
    let info = territory_info();
    let territory = info
        .territories
        .iter()
        .find(|t| t.code.eq_ignore_ascii_case(region))
        .ok_or_else(|| {
            CamoufoxError::UnknownTerritory(format!("Unknown territory: {region}"))
        })?;

    if territory.languages.is_empty() {
        return Err(CamoufoxError::LocaleError(format!(
            "No language data found for region: {region}"
        )));
    }

    let languages: Vec<String> = territory
        .languages
        .iter()
        .map(|l| l.language.replace('_', "-"))
        .collect();
    let weights: Vec<f64> = territory
        .languages
        .iter()
        .map(|l| l.population_percent)
        .collect();

    let language =
        weighted_random_choice(&languages, &weights).expect("territory has languages");
    normalize_locale(&format!("{language}-{region}"))
}

/// `SELECTOR.fromLanguage(language)`: picks a region weighted by literate
/// speaker population, then normalizes `language-REGION`.
pub fn from_language(language: &str) -> Result<Locale> {
    let info = territory_info();
    let mut regions: Vec<String> = Vec::new();
    let mut weights: Vec<f64> = Vec::new();

    for territory in &info.territories {
        if let Some(lang_pop) = territory
            .languages
            .iter()
            .find(|l| l.language.eq_ignore_ascii_case(language))
        {
            regions.push(territory.code.clone());
            weights.push(
                (lang_pop.population_percent * territory.literacy_percent / 10_000.0)
                    * territory.population,
            );
        }
    }

    if regions.is_empty() {
        return Err(CamoufoxError::UnknownLanguage(format!(
            "No region data found for language: {language}"
        )));
    }

    let region = weighted_random_choice(&regions, &weights).expect("regions non-empty");
    normalize_locale(&format!("{language}-{region}"))
}

// -- tag validation ---------------------------------------------------------------

fn is_alpha(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphabetic())
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// Structural BCP-47-ish validation (stand-in for the `language-tags` package).
pub fn is_valid_tag(tag: &str) -> bool {
    let mut parts = tag.split('-');
    let Some(language) = parts.next() else {
        return false;
    };
    // Language: 2-8 alpha.
    if !(2..=8).contains(&language.chars().count()) || !is_alpha(language) {
        return false;
    }
    for part in parts {
        if part.is_empty() {
            return false;
        }
        let len = part.chars().count();
        let ok = match len {
            4 => is_alpha(part),                          // script
            2 => is_alpha(part),                          // region
            3 => is_alpha(part) || is_digits(part),       // region (numeric) or extlang
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    true
}

/// `verifyLocale`.
pub fn verify_locale(loc: &str) -> Result<()> {
    if is_valid_tag(loc) {
        return Ok(());
    }
    Err(CamoufoxError::invalid_locale_input(loc))
}

/// `normalizeLocale`: splits a tag into language/script/region with the same
/// casing rules as the `language-tags` formatter. Region is required.
pub fn normalize_locale(locale: &str) -> Result<Locale> {
    verify_locale(locale)?;

    let parts: Vec<&str> = locale.split('-').collect();
    let language = parts[0].to_lowercase();

    let mut region: Option<String> = None;
    let mut script: Option<String> = None;
    for part in &parts[1..] {
        let len = part.chars().count();
        if len == 4 && is_alpha(part) {
            // Title case script.
            let mut chars = part.chars();
            let first = chars.next().unwrap().to_uppercase().to_string();
            script = Some(format!("{first}{}", chars.as_str().to_lowercase()));
        } else if (len == 2 && is_alpha(part)) || (len == 3 && is_digits(part)) {
            region = Some(part.to_uppercase());
        }
    }

    if region.is_none() {
        return Err(CamoufoxError::invalid_locale_input(locale));
    }

    Ok(Locale {
        language,
        region,
        script,
    })
}

/// `handleLocale`: accepts `region` (2-3 chars), `language`, `language-region`
/// or `language-script-region` inputs.
pub fn handle_locale(locale: &str, ignore_region: bool) -> Result<Locale> {
    if locale.chars().count() > 3 {
        return normalize_locale(locale);
    }

    match from_region(locale) {
        Ok(resolved) => return Ok(resolved),
        Err(e) if e.name() == "UnknownTerritory" => {}
        Err(e) => return Err(e),
    }

    if ignore_region {
        verify_locale(locale)?;
        return Ok(Locale::language_only(locale));
    }

    match from_language(locale) {
        Ok(resolved) => {
            warnings::warn_leak("no_region", None);
            return Ok(resolved);
        }
        Err(e) if e.name() == "UnknownLanguage" => {}
        Err(e) => return Err(e),
    }

    Err(CamoufoxError::invalid_locale_input(locale))
}

/// `handleLocales`: applies the first locale to the config and, when more are
/// given, fills `locale:all` with the deduplicated list.
pub fn handle_locales(locales: &[String], config: &mut Map<String, Value>) -> Result<()> {
    if locales.is_empty() {
        return Ok(());
    }

    let intl_locale = handle_locale(&locales[0], false)?.as_config()?;
    for (key, value) in intl_locale {
        config.insert(key, value);
    }

    if locales.len() < 2 {
        return Ok(());
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut all: Vec<String> = Vec::new();
    for locale in locales {
        let rendered = handle_locale(locale, true)?.as_string();
        if seen.insert(rendered.clone()) {
            all.push(rendered);
        }
    }
    config.insert("locale:all".into(), Value::String(all.join(", ")));
    Ok(())
}

/// Convenience: a full `geolocation:*` + `locale:*` config for a resolved
/// geolocation (used by the facade builder).
pub fn get_geolocation_config(geolocation: &Geolocation) -> Result<Map<String, Value>> {
    geolocation.as_config()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn territory_data_is_parsed() {
        let info = territory_info();
        assert!(info.territories.len() > 200, "CLDR has 200+ territories");
        let br = info.territories.iter().find(|t| t.code == "BR").unwrap();
        assert!(br.languages.iter().any(|l| l.language == "pt"));
        assert!(br.population > 100_000_000.0);
    }

    #[test]
    fn from_region_resolves_known_region() {
        // Weighted choice: `pt` wins ~91% for BR and `en` ~95% for US; assert
        // the dominant language holds across a sample.
        let mut br_pt = 0;
        for _ in 0..50 {
            let locale = from_region("BR").unwrap();
            assert_eq!(locale.region.as_deref(), Some("BR"));
            if locale.language == "pt" {
                br_pt += 1;
            }
        }
        assert!(br_pt >= 30, "pt should dominate BR, got {br_pt}/50");

        let mut us_en = 0;
        for _ in 0..50 {
            let locale = from_region("US").unwrap();
            assert_eq!(locale.region.as_deref(), Some("US"));
            if locale.language == "en" {
                us_en += 1;
            }
        }
        assert!(us_en >= 30, "en should dominate US, got {us_en}/50");
    }

    #[test]
    fn from_region_rejects_unknown() {
        let err = from_region("ZZ9").unwrap_err();
        assert_eq!(err.name(), "UnknownTerritory");
        assert!(err.is_locale_error());
    }

    #[test]
    fn from_language_resolves_known_language() {
        for _ in 0..10 {
            let locale = from_language("pt").unwrap();
            assert_eq!(locale.language, "pt");
            assert!(locale.region.is_some());
        }
        let err = from_language("xxq").unwrap_err();
        assert_eq!(err.name(), "UnknownLanguage");
    }

    #[test]
    fn normalize_locale_casing() {
        let locale = normalize_locale("pt-br").unwrap();
        assert_eq!(locale.language, "pt");
        assert_eq!(locale.region.as_deref(), Some("BR"));

        let locale = normalize_locale("SR-LATN-RS").unwrap();
        assert_eq!(locale.language, "sr");
        assert_eq!(locale.script.as_deref(), Some("Latn"));
        assert_eq!(locale.region.as_deref(), Some("RS"));
    }

    #[test]
    fn normalize_locale_requires_region() {
        assert!(normalize_locale("en").is_err());
        assert!(normalize_locale("en-US").is_ok());
        assert!(normalize_locale("12-34").is_err());
        assert!(normalize_locale("toolonglanguage-US").is_err());
    }

    #[test]
    fn handle_locale_full_tags() {
        let locale = handle_locale("pt-BR", false).unwrap();
        assert_eq!(locale.as_string(), "pt-BR");

        let locale = handle_locale("en-US", false).unwrap();
        assert_eq!(locale.as_string(), "en-US");
    }

    #[test]
    fn handle_locale_short_region_input() {
        // Weighted: `pt` dominates BR.
        let mut pt = 0;
        for _ in 0..20 {
            let locale = handle_locale("BR", false).unwrap();
            assert_eq!(locale.region.as_deref(), Some("BR"));
            if locale.language == "pt" {
                pt += 1;
            }
        }
        assert!(pt >= 12, "pt should dominate BR, got {pt}/20");
    }

    #[test]
    fn handle_locale_short_language_input() {
        // "pt" first resolves as region PT (Portugal), whose weighted language
        // pool is pt(96)/en(27)/fr(15)/es(10): `pt` dominates but is not
        // guaranteed. A region is always present.
        let mut pt = 0;
        for _ in 0..50 {
            let locale = handle_locale("pt", false).unwrap();
            assert!(locale.region.is_some());
            if locale.language == "pt" {
                pt += 1;
            }
        }
        assert!(pt >= 20, "pt should dominate, got {pt}/50");
    }

    #[test]
    fn handle_locale_invalid_input() {
        let err = handle_locale("zz9", false).unwrap_err();
        assert!(err.to_string().contains("Invalid locale"));
    }

    #[test]
    fn locale_as_config() {
        let locale = normalize_locale("sr-Latn-RS").unwrap();
        let config = locale.as_config().unwrap();
        assert_eq!(config.get("locale:region").unwrap(), "RS");
        assert_eq!(config.get("locale:language").unwrap(), "sr");
        assert_eq!(config.get("locale:script").unwrap(), "Latn");

        let err = Locale::language_only("en").as_config().unwrap_err();
        assert_eq!(err.name(), "LocaleError");
    }

    #[test]
    fn handle_locales_first_plus_all() {
        let mut config = Map::new();
        handle_locales(
            &["pt-BR".to_string(), "en-US".to_string(), "en-US".to_string()],
            &mut config,
        )
        .unwrap();
        assert_eq!(config.get("locale:region").unwrap(), "BR");
        assert_eq!(config.get("locale:language").unwrap(), "pt");
        assert_eq!(
            config.get("locale:all").unwrap().as_str().unwrap(),
            "pt-BR, en-US"
        );
    }

    #[test]
    fn handle_locales_single_does_not_set_all() {
        let mut config = Map::new();
        handle_locales(&["en-US".to_string()], &mut config).unwrap();
        assert!(config.get("locale:all").is_none());
    }

    #[test]
    fn geolocation_as_config() {
        let geo = Geolocation {
            locale: normalize_locale("pt-BR").unwrap(),
            longitude: -47.9,
            latitude: -15.8,
            timezone: "America/Sao_Paulo".into(),
            accuracy: Some(50),
        };
        let config = geo.as_config().unwrap();
        assert_eq!(config.get("timezone").unwrap(), "America/Sao_Paulo");
        assert_eq!(config.get("locale:region").unwrap(), "BR");
        assert_eq!(config.get("geolocation:accuracy").unwrap(), 50);
        assert!(config.get("geolocation:longitude").unwrap().as_f64().unwrap() < -47.0);
    }
}
