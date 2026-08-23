use std::{
    collections::HashMap,
    io::{self, Read, Write},
    time::Duration,
};

use serde::{Deserialize, Serialize};

const KEY_SEPARATOR: char = '\u{1f}';
const DEFAULT_HALF_LIFE_SECS: f64 = 86_400.0 * 7.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObservationKind {
    WorkspaceFocus,
    AppFocus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionObservation {
    pub at: Duration,
    pub workspace: Option<String>,
    pub app_class: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Prediction {
    pub target: String,
    pub probability: f64,
    pub decayed_samples: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct DecayedStat {
    weight: f64,
    last_seen_secs: f64,
}

impl DecayedStat {
    fn add(&mut self, now_secs: f64, half_life_secs: f64, amount: f64) {
        self.decay_to(now_secs, half_life_secs);
        self.weight += amount;
    }

    fn value_at(self, now_secs: f64, half_life_secs: f64) -> f64 {
        let elapsed = (now_secs - self.last_seen_secs).max(0.0);
        self.weight * 0.5_f64.powf(elapsed / half_life_secs)
    }

    fn decay_to(&mut self, now_secs: f64, half_life_secs: f64) {
        self.weight = self.value_at(now_secs, half_life_secs);
        self.last_seen_secs = now_secs;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionLearner {
    #[serde(default = "default_half_life")]
    half_life_secs: f64,
    #[serde(default)]
    workspace_transitions: HashMap<String, DecayedStat>,
    #[serde(default)]
    app_transitions: HashMap<String, DecayedStat>,
    #[serde(default)]
    workspace_focuses: HashMap<String, DecayedStat>,
    #[serde(default)]
    app_focuses: HashMap<String, DecayedStat>,
    #[serde(default)]
    workspace_dwell_secs: HashMap<String, DecayedStat>,
    #[serde(default)]
    app_dwell_secs: HashMap<String, DecayedStat>,
    #[serde(skip)]
    current_workspace: Option<ActiveFocus>,
    #[serde(skip)]
    current_app: Option<ActiveFocus>,
}

#[derive(Debug, Clone)]
struct ActiveFocus {
    name: String,
    started_at: Duration,
}

impl Default for AttentionLearner {
    fn default() -> Self {
        Self {
            half_life_secs: default_half_life(),
            workspace_transitions: HashMap::new(),
            app_transitions: HashMap::new(),
            workspace_focuses: HashMap::new(),
            app_focuses: HashMap::new(),
            workspace_dwell_secs: HashMap::new(),
            app_dwell_secs: HashMap::new(),
            current_workspace: None,
            current_app: None,
        }
    }
}

impl AttentionLearner {
    #[must_use]
    pub fn with_half_life(half_life: Duration) -> Self {
        Self {
            half_life_secs: half_life.as_secs_f64().max(1.0),
            ..Self::default()
        }
    }

    pub fn observe(&mut self, observation: AttentionObservation) {
        if let Some(workspace) = observation.workspace {
            self.observe_workspace(workspace, observation.at);
        }
        if let Some(app_class) = observation.app_class {
            self.observe_app(app_class, observation.at);
        }
    }

    pub fn finish_active(&mut self, at: Duration) {
        if let Some(active) = self.current_workspace.take() {
            add_dwell(
                &mut self.workspace_dwell_secs,
                &active,
                at,
                self.half_life_secs,
            );
        }
        if let Some(active) = self.current_app.take() {
            add_dwell(&mut self.app_dwell_secs, &active, at, self.half_life_secs);
        }
    }

    #[must_use]
    pub fn predict_next_workspaces(
        &self,
        current: &str,
        at: Duration,
        limit: usize,
    ) -> Vec<Prediction> {
        predict_transitions(
            &self.workspace_transitions,
            current,
            at.as_secs_f64(),
            self.half_life_secs,
            limit,
        )
    }

    #[must_use]
    pub fn predict_next_apps(&self, current: &str, at: Duration, limit: usize) -> Vec<Prediction> {
        predict_transitions(
            &self.app_transitions,
            current,
            at.as_secs_f64(),
            self.half_life_secs,
            limit,
        )
    }

    #[must_use]
    pub fn workspace_relevance(&self, workspace: &str, at: Duration) -> f64 {
        relevance(
            &self.workspace_focuses,
            &self.workspace_dwell_secs,
            workspace,
            at.as_secs_f64(),
            self.half_life_secs,
        )
    }

    #[must_use]
    pub fn app_relevance(&self, app_class: &str, at: Duration) -> f64 {
        relevance(
            &self.app_focuses,
            &self.app_dwell_secs,
            app_class,
            at.as_secs_f64(),
            self.half_life_secs,
        )
    }

    /// Serializes learned statistics as JSON.
    ///
    /// # Errors
    ///
    /// Returns an error when serialization or writing fails.
    pub fn save_json(&self, mut writer: impl Write) -> io::Result<()> {
        serde_json::to_writer_pretty(&mut writer, self)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        writer.write_all(b"\n")
    }

    /// Loads learned statistics from JSON.
    ///
    /// # Errors
    ///
    /// Returns an error when reading or decoding the state fails.
    pub fn load_json(mut reader: impl Read) -> io::Result<Self> {
        let mut content = String::new();
        reader.read_to_string(&mut content)?;
        serde_json::from_str(&content)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    fn observe_workspace(&mut self, workspace: String, at: Duration) {
        let now_secs = at.as_secs_f64();
        if self
            .current_workspace
            .as_ref()
            .is_some_and(|active| active.name == workspace)
        {
            return;
        }

        if let Some(previous) = self.current_workspace.take() {
            add_dwell(
                &mut self.workspace_dwell_secs,
                &previous,
                at,
                self.half_life_secs,
            );
            add_transition(
                &mut self.workspace_transitions,
                &previous.name,
                &workspace,
                now_secs,
                self.half_life_secs,
            );
        }

        add_stat(
            &mut self.workspace_focuses,
            &workspace,
            now_secs,
            self.half_life_secs,
            1.0,
        );
        self.current_workspace = Some(ActiveFocus {
            name: workspace,
            started_at: at,
        });
    }

    fn observe_app(&mut self, app_class: String, at: Duration) {
        let now_secs = at.as_secs_f64();
        if self
            .current_app
            .as_ref()
            .is_some_and(|active| active.name == app_class)
        {
            return;
        }

        if let Some(previous) = self.current_app.take() {
            add_dwell(&mut self.app_dwell_secs, &previous, at, self.half_life_secs);
            add_transition(
                &mut self.app_transitions,
                &previous.name,
                &app_class,
                now_secs,
                self.half_life_secs,
            );
        }

        add_stat(
            &mut self.app_focuses,
            &app_class,
            now_secs,
            self.half_life_secs,
            1.0,
        );
        self.current_app = Some(ActiveFocus {
            name: app_class,
            started_at: at,
        });
    }
}

fn default_half_life() -> f64 {
    DEFAULT_HALF_LIFE_SECS
}

fn add_transition(
    stats: &mut HashMap<String, DecayedStat>,
    from: &str,
    to: &str,
    now_secs: f64,
    half_life_secs: f64,
) {
    if from == to {
        return;
    }
    let key = transition_key(from, to);
    add_stat(stats, &key, now_secs, half_life_secs, 1.0);
}

fn add_dwell(
    stats: &mut HashMap<String, DecayedStat>,
    active: &ActiveFocus,
    at: Duration,
    half_life_secs: f64,
) {
    let dwell = at.saturating_sub(active.started_at).as_secs_f64();
    if dwell > 0.0 {
        add_stat(stats, &active.name, at.as_secs_f64(), half_life_secs, dwell);
    }
}

fn add_stat(
    stats: &mut HashMap<String, DecayedStat>,
    key: &str,
    now_secs: f64,
    half_life_secs: f64,
    amount: f64,
) {
    stats
        .entry(key.to_owned())
        .or_insert(DecayedStat {
            weight: 0.0,
            last_seen_secs: now_secs,
        })
        .add(now_secs, half_life_secs, amount);
}

fn predict_transitions(
    stats: &HashMap<String, DecayedStat>,
    current: &str,
    now_secs: f64,
    half_life_secs: f64,
    limit: usize,
) -> Vec<Prediction> {
    let prefix = format!("{current}{KEY_SEPARATOR}");
    let mut candidates: Vec<(String, f64)> = stats
        .iter()
        .filter_map(|(key, stat)| {
            let target = key.strip_prefix(&prefix)?;
            let weight = stat.value_at(now_secs, half_life_secs);
            (weight > f64::EPSILON).then(|| (target.to_owned(), weight))
        })
        .collect();

    let total: f64 = candidates.iter().map(|(_, weight)| *weight).sum();
    if total <= f64::EPSILON {
        return Vec::new();
    }

    candidates.sort_by(|left, right| right.1.total_cmp(&left.1));
    candidates
        .into_iter()
        .take(limit)
        .map(|(target, weight)| Prediction {
            target,
            probability: weight / total,
            decayed_samples: weight,
        })
        .collect()
}

fn relevance(
    focuses: &HashMap<String, DecayedStat>,
    dwell: &HashMap<String, DecayedStat>,
    name: &str,
    now_secs: f64,
    half_life_secs: f64,
) -> f64 {
    let focus_weight = focuses
        .get(name)
        .map_or(0.0, |stat| stat.value_at(now_secs, half_life_secs));
    let dwell_seconds = dwell
        .get(name)
        .map_or(0.0, |stat| stat.value_at(now_secs, half_life_secs));

    focus_weight + (dwell_seconds / 60.0).sqrt()
}

fn transition_key(from: &str, to: &str) -> String {
    format!("{from}{KEY_SEPARATOR}{to}")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{AttentionLearner, AttentionObservation};

    fn observe(learner: &mut AttentionLearner, seconds: u64, workspace: &str, app: &str) {
        learner.observe(AttentionObservation {
            at: Duration::from_secs(seconds),
            workspace: Some(workspace.to_owned()),
            app_class: Some(app.to_owned()),
        });
    }

    #[test]
    fn learns_workspace_transition_probabilities() {
        let mut learner = AttentionLearner::with_half_life(Duration::from_secs(10_000));
        observe(&mut learner, 0, "dev", "zed");
        observe(&mut learner, 10, "web", "firefox");
        observe(&mut learner, 20, "dev", "zed");
        observe(&mut learner, 30, "web", "firefox");
        observe(&mut learner, 40, "dev", "zed");
        observe(&mut learner, 50, "chat", "discord");

        let predictions = learner.predict_next_workspaces("dev", Duration::from_secs(50), 3);
        assert_eq!(predictions.len(), 2);
        assert_eq!(predictions[0].target, "web");
        assert!(predictions[0].probability > predictions[1].probability);
    }

    #[test]
    fn repeated_same_focus_does_not_create_fake_transitions() {
        let mut learner = AttentionLearner::default();
        observe(&mut learner, 0, "dev", "zed");
        observe(&mut learner, 5, "dev", "zed");
        observe(&mut learner, 10, "web", "firefox");

        let predictions = learner.predict_next_workspaces("dev", Duration::from_secs(10), 3);
        assert_eq!(predictions.len(), 1);
        assert_eq!(predictions[0].target, "web");
    }

    #[test]
    fn old_observations_decay() {
        let mut learner = AttentionLearner::with_half_life(Duration::from_secs(10));
        observe(&mut learner, 0, "dev", "zed");
        observe(&mut learner, 1, "web", "firefox");
        observe(&mut learner, 100, "dev", "zed");
        observe(&mut learner, 101, "chat", "discord");

        let predictions = learner.predict_next_workspaces("dev", Duration::from_secs(101), 2);
        assert_eq!(predictions[0].target, "chat");
        assert!(predictions[0].probability > 0.9);
    }

    #[test]
    fn json_round_trip_preserves_statistics_but_not_active_focus() {
        let mut learner = AttentionLearner::default();
        observe(&mut learner, 10, "dev", "zed");
        observe(&mut learner, 20, "web", "firefox");

        let mut json = Vec::new();
        learner.save_json(&mut json).expect("serialize learner");
        let restored = AttentionLearner::load_json(json.as_slice()).expect("deserialize learner");

        let predictions = restored.predict_next_workspaces("dev", Duration::from_secs(20), 1);
        assert_eq!(predictions[0].target, "web");
    }
}
