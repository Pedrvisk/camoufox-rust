//! Persona rotation policies.
//!
//! Personas are stable identities; rotation policies decide *when* to move
//! on to a fresh one — per domain (each site sees its own identity), after
//! a period of time (limited exposure per identity), or after N uses.
//!
//! The policy is pure: given a list of existing personas and the current
//! context (domain, time, use counters), it picks the next persona to use
//! and whether the current one should be retired.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::persona::PersonaRecord;

/// Why a persona was selected (or why rotation is required).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum RotationDecision {
    /// Keep using the current persona.
    Keep {
        /// The persona id to keep using.
        persona_id: String,
    },
    /// Switch to another persona.
    Rotate {
        /// The persona id to switch to.
        persona_id: String,
        /// Human-readable rotation cause.
        cause: String,
    },
    /// No persona satisfies the policy; generate a new one.
    Generate {
        /// Suggested id for the new persona.
        suggested_id: String,
        /// Human-readable rotation cause.
        cause: String,
    },
}

/// A persona rotation policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum RotationPolicy {
    /// Never rotates: the first persona is reused forever.
    None,
    /// A distinct persona per domain.
    PerDomain,
    /// Rotate after the persona has been in use for `max_age_secs`.
    TimeBased {
        /// Maximum age in seconds before rotating.
        max_age_secs: u64,
    },
    /// Rotate after the persona has been used `max_uses` times.
    UsageBased {
        /// Maximum number of uses before rotating.
        max_uses: u64,
    },
    /// Combines policies: rotates as soon as any of them triggers.
    Any {
        /// Inner policies.
        policies: Vec<RotationPolicy>,
    },
}

/// Use counters tracked per persona.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RotationState {
    /// persona id → number of launches.
    pub uses: std::collections::BTreeMap<String, u64>,
}

impl RotationState {
    /// Records one use of `persona_id`.
    pub fn record_use(&mut self, persona_id: &str) {
        *self.uses.entry(persona_id.to_string()).or_insert(0) += 1;
    }

    /// Use count for `persona_id`.
    pub fn uses(&self, persona_id: &str) -> u64 {
        self.uses.get(persona_id).copied().unwrap_or(0)
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// The rotation context: which persona is active and for what.
#[derive(Debug, Clone)]
pub struct RotationContext<'a> {
    /// The persona currently in use, if any.
    pub current: Option<&'a PersonaRecord>,
    /// All personas available (including `current`).
    pub pool: &'a [PersonaRecord],
    /// Tracked use counters.
    pub state: &'a RotationState,
    /// The domain the next launch targets (per-domain policies).
    pub domain: Option<&'a str>,
    /// Mapping persona id → domains it has already been used on.
    pub persona_domains: &'a std::collections::BTreeMap<String, Vec<String>>,
}

impl RotationPolicy {
    /// Decides whether to keep, rotate or generate a persona.
    pub fn decide(&self, ctx: &RotationContext<'_>) -> RotationDecision {
        match self {
            RotationPolicy::None => match ctx.current {
                Some(current) => RotationDecision::Keep {
                    persona_id: current.id.clone(),
                },
                // Nothing to keep: pick (or create) the first persona.
                None => first_available(ctx).unwrap_or(RotationDecision::Generate {
                    suggested_id: default_suggested_id(ctx),
                    cause: "no persona in the pool".into(),
                }),
            },
            RotationPolicy::PerDomain => decide_per_domain(ctx),
            RotationPolicy::TimeBased { max_age_secs } => {
                decide_time_based(ctx, *max_age_secs)
            }
            RotationPolicy::UsageBased { max_uses } => decide_usage_based(ctx, *max_uses),
            RotationPolicy::Any { policies } => {
                for policy in policies {
                    let decision = policy.decide(ctx);
                    if !matches!(decision, RotationDecision::Keep { .. }) {
                        return decision;
                    }
                }
                match ctx.current {
                    Some(current) => RotationDecision::Keep {
                        persona_id: current.id.clone(),
                    },
                    None => first_available(ctx).unwrap_or(RotationDecision::Generate {
                        suggested_id: default_suggested_id(ctx),
                        cause: "no persona in the pool".into(),
                    }),
                }
            }
        }
    }
}

fn first_available(ctx: &RotationContext<'_>) -> Option<RotationDecision> {
    ctx.pool
        .first()
        .map(|record| RotationDecision::Keep {
            persona_id: record.id.clone(),
        })
}

fn default_suggested_id(ctx: &RotationContext<'_>) -> String {
    let domain = ctx.domain.unwrap_or("default");
    format!("persona-{}", slug(domain))
}

/// `PerDomain`: keep the persona assigned to this domain; assign an unused
/// one otherwise; generate a fresh one when the pool is exhausted.
fn decide_per_domain(ctx: &RotationContext<'_>) -> RotationDecision {
    let Some(domain) = ctx.domain else {
        // No domain known: behave like `None`.
        return match ctx.current {
            Some(current) => RotationDecision::Keep {
                persona_id: current.id.clone(),
            },
            None => first_available(ctx).unwrap_or(RotationDecision::Generate {
                suggested_id: default_suggested_id(ctx),
                cause: "no persona in the pool".into(),
            }),
        };
    };
    let domain = &domain.to_ascii_lowercase();

    // Persona already used on this domain: keep it (sticky identity).
    let assigned = ctx
        .persona_domains
        .iter()
        .find(|(_, domains)| domains.iter().any(|d| d.eq_ignore_ascii_case(domain)))
        .map(|(id, _)| id.clone());
    if let Some(persona_id) = assigned {
        return RotationDecision::Keep { persona_id };
    }

    // Prefer a persona never seen on any domain.
    let fresh = ctx
        .pool
        .iter()
        .find(|record| !ctx.persona_domains.contains_key(&record.id))
        .map(|record| record.id.clone());
    if let Some(persona_id) = fresh {
        return RotationDecision::Rotate {
            persona_id,
            cause: format!("persona not yet used on '{domain}'"),
        };
    }

    // All personas have domain history; reuse the one with the fewest
    // domains (spreads identities across sites).
    let least_used = ctx
        .persona_domains
        .iter()
        .min_by_key(|(_, domains)| domains.len())
        .map(|(id, _)| id.clone());
    if let Some(persona_id) = least_used {
        return RotationDecision::Rotate {
            persona_id,
            cause: format!("all personas already assigned; reusing the least spread one for '{domain}'"),
        };
    }

    RotationDecision::Generate {
        suggested_id: format!("persona-{}", slug(domain)),
        cause: format!("no persona available for '{domain}'"),
    }
}

fn decide_time_based(ctx: &RotationContext<'_>, max_age_secs: u64) -> RotationDecision {
    let now = now_secs();
    if let Some(current) = ctx.current {
        let age = now.saturating_sub(current.created_at);
        if age <= max_age_secs {
            return RotationDecision::Keep {
                persona_id: current.id.clone(),
            };
        }
        // Current persona is stale: prefer a fresh one from the pool.
        if let Some(fresh) = ctx.pool.iter().find(|record| {
            now.saturating_sub(record.created_at) <= max_age_secs
                && record.id != current.id
        }) {
            return RotationDecision::Rotate {
                persona_id: fresh.id.clone(),
                cause: format!(
                    "persona '{}' is {}s old (max {max_age_secs}s)",
                    current.id, age
                ),
            };
        }
        return RotationDecision::Generate {
            suggested_id: format!("persona-{now}"),
            cause: format!(
                "persona '{}' is {}s old (max {max_age_secs}s) and no fresh persona is available",
                current.id, age
            ),
        };
    }
    first_available(ctx).unwrap_or(RotationDecision::Generate {
        suggested_id: format!("persona-{now}"),
        cause: "no current persona".into(),
    })
}

fn decide_usage_based(ctx: &RotationContext<'_>, max_uses: u64) -> RotationDecision {
    if let Some(current) = ctx.current {
        let uses = ctx.state.uses(&current.id);
        if uses < max_uses {
            return RotationDecision::Keep {
                persona_id: current.id.clone(),
            };
        }
        // Exhausted: prefer the least-used other persona under the cap.
        if let Some((id, uses)) = ctx
            .pool
            .iter()
            .filter(|record| record.id != current.id)
            .map(|record| (record.id.clone(), ctx.state.uses(&record.id)))
            .min_by_key(|(_, uses)| *uses)
            .filter(|(_, uses)| *uses < max_uses)
        {
            let _ = uses;
            return RotationDecision::Rotate {
                persona_id: id,
                cause: format!(
                    "persona '{}' hit {} uses (max {max_uses})",
                    current.id, uses
                ),
            };
        }
        return RotationDecision::Generate {
            suggested_id: format!("persona-{}", ctx.state.uses.len() + 1),
            cause: format!(
                "persona '{}' hit {} uses (max {max_uses}) and no persona has headroom",
                current.id, uses
            ),
        };
    }
    first_available(ctx).unwrap_or(RotationDecision::Generate {
        suggested_id: "persona-1".into(),
        cause: "no current persona".into(),
    })
}

/// Lowercases and keeps only `[a-z0-9._-]`, like persona ids.
fn slug(input: &str) -> String {
    let slug: String = input
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = slug.trim_matches('-');
    if trimmed.is_empty() {
        "default".into()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::FingerprintRequest;

    fn persona(id: &str, created_at: u64) -> PersonaRecord {
        let mut record =
            PersonaRecord::generate(id, &FingerprintRequest::default()).unwrap();
        record.created_at = created_at;
        record
    }

    fn ctx<'a>(
        current: Option<&'a PersonaRecord>,
        pool: &'a [PersonaRecord],
        state: &'a RotationState,
        domain: Option<&'a str>,
        persona_domains: &'a std::collections::BTreeMap<String, Vec<String>>,
    ) -> RotationContext<'a> {
        RotationContext {
            current,
            pool,
            state,
            domain,
            persona_domains,
        }
    }

    #[test]
    fn none_policy_keeps_current() {
        let current = persona("a", 1000);
        let state = RotationState::default();
        let decision = RotationPolicy::None.decide(&ctx(Some(&current), std::slice::from_ref(&current), &state, None, &Default::default()));
        assert_eq!(
            decision,
            RotationDecision::Keep {
                persona_id: "a".into()
            }
        );
    }

    #[test]
    fn per_domain_is_sticky() {
        let a = persona("a", 1000);
        let b = persona("b", 1000);
        let state = RotationState::default();
        let mut domains = std::collections::BTreeMap::new();
        domains.insert("a".to_string(), vec!["example.com".to_string()]);

        // example.com → keep a (sticky)
        let decision = RotationPolicy::PerDomain.decide(&ctx(
            Some(&b),
            &[a.clone(), b.clone()],
            &state,
            Some("example.com"),
            &domains,
        ));
        assert_eq!(
            decision,
            RotationDecision::Keep {
                persona_id: "a".into()
            }
        );

        // other.com → rotate to the persona without history (b)
        let decision = RotationPolicy::PerDomain.decide(&ctx(
            Some(&a),
            &[a.clone(), b.clone()],
            &state,
            Some("other.com"),
            &domains,
        ));
        assert!(matches!(decision, RotationDecision::Rotate { ref persona_id, .. } if persona_id == "b"));
    }

    #[test]
    fn time_based_rotates_stale() {
        let now = now_secs();
        let old = persona("old", now - 10_000);
        let fresh = persona("fresh", now);
        let state = RotationState::default();
        let decision = RotationPolicy::TimeBased {
            max_age_secs: 3600,
        }
        .decide(&ctx(
            Some(&old),
            &[old.clone(), fresh.clone()],
            &state,
            None,
            &Default::default(),
        ));
        assert!(matches!(decision, RotationDecision::Rotate { ref persona_id, .. } if persona_id == "fresh"));

        // Current still fresh: keep.
        let current = fresh;
        let decision = RotationPolicy::TimeBased {
            max_age_secs: 3600,
        }
        .decide(&ctx(
            Some(&current),
            std::slice::from_ref(&current),
            &state,
            None,
            &Default::default(),
        ));
        assert!(matches!(decision, RotationDecision::Keep { .. }));
    }

    #[test]
    fn usage_based_rotates_after_cap() {
        let a = persona("a", 1000);
        let b = persona("b", 1000);
        let mut state = RotationState::default();
        state.record_use("a");
        state.record_use("a");
        state.record_use("a");
        state.record_use("b");

        let decision = RotationPolicy::UsageBased { max_uses: 3 }.decide(&ctx(
            Some(&a),
            &[a.clone(), b.clone()],
            &state,
            None,
            &Default::default(),
        ));
        assert!(matches!(decision, RotationDecision::Rotate { ref persona_id, .. } if persona_id == "b"));

        // Under the cap: keep.
        let decision = RotationPolicy::UsageBased { max_uses: 5 }.decide(&ctx(
            Some(&b),
            std::slice::from_ref(&b),
            &state,
            None,
            &Default::default(),
        ));
        assert!(matches!(decision, RotationDecision::Keep { .. }));
    }

    #[test]
    fn any_combines_policies() {
        let now = now_secs();
        let old = persona("old", now - 10_000);
        let fresh = persona("fresh", now);
        let state = RotationState::default();
        let combined = RotationPolicy::Any {
            policies: vec![
                RotationPolicy::UsageBased { max_uses: 10 },
                RotationPolicy::TimeBased {
                    max_age_secs: 3600,
                },
            ],
        };
        let decision = combined.decide(&ctx(
            Some(&old),
            &[old.clone(), fresh.clone()],
            &state,
            None,
            &Default::default(),
        ));
        // Time policy triggers (old persona) before usage does.
        assert!(matches!(decision, RotationDecision::Rotate { ref persona_id, .. } if persona_id == "fresh"));

        // Neither triggers: keep.
        let decision = combined.decide(&ctx(
            Some(&fresh),
            std::slice::from_ref(&fresh),
            &state,
            None,
            &Default::default(),
        ));
        assert!(matches!(decision, RotationDecision::Keep { .. }));
    }

    #[test]
    fn slug_sanitizes_domains() {
        assert_eq!(slug("Example.COM"), "example.com");
        assert_eq!(slug("sub.example.com/path"), "sub.example.com-path");
        assert_eq!(slug("---"), "default");
    }

    #[test]
    fn generated_suggestions_use_domain() {
        let a = persona("a", 1000);
        let state = RotationState::default();
        let empty: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        let decision = RotationPolicy::PerDomain.decide(&ctx(
            Some(&a),
            std::slice::from_ref(&a),
            &state,
            Some("News.Example.com"),
            &empty,
        ));
        // 'a' is in the pool but has no domain history → rotate to it.
        assert!(matches!(decision, RotationDecision::Rotate { .. }));

        // Empty pool: generate a persona named after the domain.
        let empty_pool: Vec<PersonaRecord> = Vec::new();
        let decision = RotationPolicy::PerDomain.decide(&ctx(
            None,
            &empty_pool,
            &state,
            Some("News.Example.com"),
            &empty,
        ));
        match decision {
            RotationDecision::Generate { suggested_id, .. } => {
                assert_eq!(suggested_id, "persona-news.example.com");
            }
            other => panic!("expected Generate, got {other:?}"),
        }
    }
}
