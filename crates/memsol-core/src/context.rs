use std::{
    collections::{HashMap, VecDeque},
    io::{self, Read, Write},
    time::Duration,
};

use serde::{Deserialize, Serialize};

const KEY_SEPARATOR: char = '\u{1f}';
const DEFAULT_HALF_LIFE_SECS: f64 = 86_400.0 * 7.0;
const DEFAULT_EVENT_WINDOW_SECS: f64 = 120.0;
const DEFAULT_RESPONSE_HALF_LIFE_SECS: f64 = 12.0;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextEvent {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl ContextEvent {
    #[must_use]
    pub fn key(&self) -> String {
        self.source.as_ref().map_or_else(
            || self.kind.clone(),
            |source| format!("{}:{source}", self.kind),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AttentionContext {
    pub workspace: Option<String>,
    pub app_class: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContextPrediction {
    pub target: String,
    pub probability: f64,
    pub decayed_samples: f64,
    pub event: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct DecayedStat {
    weight: f64,
    last_seen_secs: f64,
}

impl DecayedStat {
    fn add(&mut self, now_secs: f64, half_life_secs: f64, amount: f64) {
        self.weight = self.value_at(now_secs, half_life_secs) + amount;
        self.last_seen_secs = now_secs;
    }

    fn value_at(self, now_secs: f64, half_life_secs: f64) -> f64 {
        let elapsed = (now_secs - self.last_seen_secs).max(0.0);
        self.weight * 0.5_f64.powf(elapsed / half_life_secs)
    }
}

#[derive(Debug, Clone)]
struct PendingEvent {
    key: String,
    at: Duration,
    context: AttentionContext,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextLearner {
    #[serde(default = "default_half_life")]
    half_life_secs: f64,
    #[serde(default = "default_event_window")]
    event_window_secs: f64,
    #[serde(default = "default_response_half_life")]
    response_half_life_secs: f64,
    #[serde(default)]
    workspace_outcomes: HashMap<String, DecayedStat>,
    #[serde(default)]
    app_outcomes: HashMap<String, DecayedStat>,
    #[serde(skip)]
    pending: VecDeque<PendingEvent>,
}

impl Default for ContextLearner {
    fn default() -> Self {
        Self {
            half_life_secs: default_half_life(),
            event_window_secs: default_event_window(),
            response_half_life_secs: default_response_half_life(),
            workspace_outcomes: HashMap::new(),
            app_outcomes: HashMap::new(),
            pending: VecDeque::new(),
        }
    }
}

impl ContextLearner {
    #[must_use]
    pub fn with_timings(
        half_life: Duration,
        event_window: Duration,
        response_half_life: Duration,
    ) -> Self {
        Self {
            half_life_secs: half_life.as_secs_f64().max(1.0),
            event_window_secs: event_window.as_secs_f64().max(1.0),
            response_half_life_secs: response_half_life.as_secs_f64().max(0.1),
            ..Self::default()
        }
    }

    pub fn observe_event(&mut self, event: ContextEvent, at: Duration, context: AttentionContext) {
        self.prune(at);
        self.pending.push_back(PendingEvent {
            key: event.key(),
            at,
            context,
        });
    }

    pub fn observe_attention(&mut self, context: &AttentionContext, at: Duration) {
        self.prune(at);
        let now_secs = at.as_secs_f64();

        for pending in &self.pending {
            let age = at.saturating_sub(pending.at).as_secs_f64();
            let response_weight = 0.5_f64.powf(age / self.response_half_life_secs);

            if let Some(workspace) = context.workspace.as_deref() {
                for key in contextual_keys(&pending.key, &pending.context) {
                    add_outcome(
                        &mut self.workspace_outcomes,
                        &key,
                        workspace,
                        now_secs,
                        self.half_life_secs,
                        response_weight,
                    );
                }
            }

            if let Some(app_class) = context.app_class.as_deref() {
                for key in contextual_keys(&pending.key, &pending.context) {
                    add_outcome(
                        &mut self.app_outcomes,
                        &key,
                        app_class,
                        now_secs,
                        self.half_life_secs,
                        response_weight,
                    );
                }
            }
        }
    }

    #[must_use]
    pub fn predict_workspaces(
        &self,
        context: &AttentionContext,
        at: Duration,
        limit: usize,
    ) -> Vec<ContextPrediction> {
        self.predict(&self.workspace_outcomes, context, at, limit)
    }

    #[must_use]
    pub fn predict_apps(
        &self,
        context: &AttentionContext,
        at: Duration,
        limit: usize,
    ) -> Vec<ContextPrediction> {
        self.predict(&self.app_outcomes, context, at, limit)
    }

    /// Serializes learned contextual statistics as JSON.
    ///
    /// # Errors
    /// Returns an error when serialization or writing fails.
    pub fn save_json(&self, mut writer: impl Write) -> io::Result<()> {
        serde_json::to_writer_pretty(&mut writer, self)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        writer.write_all(b"\n")
    }

    /// Loads learned contextual statistics from JSON.
    ///
    /// # Errors
    /// Returns an error when reading or decoding state fails.
    pub fn load_json(mut reader: impl Read) -> io::Result<Self> {
        let mut content = String::new();
        reader.read_to_string(&mut content)?;
        serde_json::from_str(&content)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    fn predict(
        &self,
        stats: &HashMap<String, DecayedStat>,
        context: &AttentionContext,
        at: Duration,
        limit: usize,
    ) -> Vec<ContextPrediction> {
        let now_secs = at.as_secs_f64();
        let mut combined: HashMap<String, (f64, String)> = HashMap::new();

        for pending in &self.pending {
            let age = at.saturating_sub(pending.at).as_secs_f64();
            if age > self.event_window_secs {
                continue;
            }
            let event_weight = 0.5_f64.powf(age / self.response_half_life_secs);
            let prediction_context = AttentionContext {
                workspace: context
                    .workspace
                    .clone()
                    .or_else(|| pending.context.workspace.clone()),
                app_class: context
                    .app_class
                    .clone()
                    .or_else(|| pending.context.app_class.clone()),
            };

            let keys = contextual_keys(&pending.key, &prediction_context);
            for (specificity, key) in keys.into_iter().enumerate() {
                let prefix = format!("{key}{KEY_SEPARATOR}");
                let specificity_weight = 1.0 + specificity as f64;
                for (stored_key, stat) in stats {
                    let Some(target) = stored_key.strip_prefix(&prefix) else {
                        continue;
                    };
                    let weight = stat.value_at(now_secs, self.half_life_secs)
                        * event_weight
                        * specificity_weight;
                    if weight > f64::EPSILON {
                        let entry = combined
                            .entry(target.to_owned())
                            .or_insert((0.0, pending.key.clone()));
                        entry.0 += weight;
                    }
                }
            }
        }

        let total: f64 = combined.values().map(|(weight, _)| *weight).sum();
        if total <= f64::EPSILON {
            return Vec::new();
        }

        let mut predictions: Vec<_> = combined
            .into_iter()
            .map(|(target, (weight, event))| ContextPrediction {
                target,
                probability: weight / total,
                decayed_samples: weight,
                event,
            })
            .collect();
        predictions.sort_by(|left, right| right.decayed_samples.total_cmp(&left.decayed_samples));
        predictions.truncate(limit);
        predictions
    }

    fn prune(&mut self, at: Duration) {
        while self.pending.front().is_some_and(|event| {
            at.saturating_sub(event.at).as_secs_f64() > self.event_window_secs
        }) {
            self.pending.pop_front();
        }
    }
}

fn contextual_keys(event: &str, context: &AttentionContext) -> Vec<String> {
    let mut keys = vec![format!("{event}{KEY_SEPARATOR}*{KEY_SEPARATOR}*")];
    if let Some(workspace) = context.workspace.as_deref() {
        keys.push(format!("{event}{KEY_SEPARATOR}{workspace}{KEY_SEPARATOR}*"));
        if let Some(app) = context.app_class.as_deref() {
            keys.push(format!("{event}{KEY_SEPARATOR}{workspace}{KEY_SEPARATOR}{app}"));
        }
    }
    keys
}

fn add_outcome(
    stats: &mut HashMap<String, DecayedStat>,
    context_key: &str,
    target: &str,
    now_secs: f64,
    half_life_secs: f64,
    amount: f64,
) {
    let key = format!("{context_key}{KEY_SEPARATOR}{target}");
    stats
        .entry(key)
        .or_insert(DecayedStat {
            weight: 0.0,
            last_seen_secs: now_secs,
        })
        .add(now_secs, half_life_secs, amount);
}

fn default_half_life() -> f64 {
    DEFAULT_HALF_LIFE_SECS
}

fn default_event_window() -> f64 {
    DEFAULT_EVENT_WINDOW_SECS
}

fn default_response_half_life() -> f64 {
    DEFAULT_RESPONSE_HALF_LIFE_SECS
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{AttentionContext, ContextEvent, ContextLearner};

    fn context(workspace: &str, app: &str) -> AttentionContext {
        AttentionContext {
            workspace: Some(workspace.to_owned()),
            app_class: Some(app.to_owned()),
        }
    }

    #[test]
    fn learns_event_conditioned_workspace() {
        let mut learner = ContextLearner::with_timings(
            Duration::from_secs(10_000),
            Duration::from_secs(120),
            Duration::from_secs(20),
        );
        let dev = context("dev", "zed");
        let chat = context("chat", "discord");

        for offset in [0, 100, 200] {
            learner.observe_event(
                ContextEvent {
                    kind: "notification".to_owned(),
                    source: Some("discord".to_owned()),
                },
                Duration::from_secs(offset),
                dev.clone(),
            );
            learner.observe_attention(&chat, Duration::from_secs(offset + 2));
        }

        learner.observe_event(
            ContextEvent {
                kind: "notification".to_owned(),
                source: Some("discord".to_owned()),
            },
            Duration::from_secs(300),
            dev.clone(),
        );

        let predictions = learner.predict_workspaces(&dev, Duration::from_secs(301), 3);
        assert_eq!(predictions[0].target, "chat");
        assert!(predictions[0].probability > 0.9);
    }

    #[test]
    fn recent_events_outweigh_stale_events() {
        let mut learner = ContextLearner::with_timings(
            Duration::from_secs(10_000),
            Duration::from_secs(120),
            Duration::from_secs(5),
        );
        let dev = context("dev", "zed");
        let web = context("web", "firefox");

        learner.observe_event(
            ContextEvent {
                kind: "build_finished".to_owned(),
                source: None,
            },
            Duration::from_secs(0),
            dev.clone(),
        );
        learner.observe_attention(&web, Duration::from_secs(1));
        learner.observe_event(
            ContextEvent {
                kind: "build_finished".to_owned(),
                source: None,
            },
            Duration::from_secs(100),
            dev.clone(),
        );

        assert!(!learner
            .predict_workspaces(&dev, Duration::from_secs(110), 1)
            .is_empty());
        assert!(learner
            .predict_workspaces(&dev, Duration::from_secs(230), 1)
            .is_empty());
    }

    #[test]
    fn persistence_does_not_restore_pending_events() {
        let mut learner = ContextLearner::default();
        learner.observe_event(
            ContextEvent {
                kind: "notification".to_owned(),
                source: Some("discord".to_owned()),
            },
            Duration::from_secs(10),
            context("dev", "zed"),
        );
        learner.observe_attention(&context("chat", "discord"), Duration::from_secs(12));

        let mut json = Vec::new();
        learner.save_json(&mut json).expect("save context learner");
        let restored = ContextLearner::load_json(json.as_slice()).expect("load context learner");

        assert!(restored
            .predict_workspaces(&context("dev", "zed"), Duration::from_secs(12), 1)
            .is_empty());
    }
}
