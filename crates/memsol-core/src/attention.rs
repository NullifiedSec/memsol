use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionState {
    Focused,
    Visible,
    Hidden,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowState {
    pub address: String,
    pub workspace: String,
    pub class: String,
    pub title: String,
    pub monitor: Option<String>,
    pub mapped: bool,
    pub hidden: bool,
    pub fullscreen: bool,
    pub attention: AttentionState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceState {
    pub id: i64,
    pub name: String,
    pub monitor: Option<String>,
    pub active: bool,
    pub window_count: usize,
}

#[derive(Debug, Clone, Default)]
pub struct AttentionGraph {
    windows: HashMap<String, WindowState>,
    workspaces: HashMap<String, WorkspaceState>,
    active_workspaces: HashMap<String, String>,
    focused_window: Option<String>,
}

impl AttentionGraph {
    #[must_use]
    pub fn windows(&self) -> &HashMap<String, WindowState> {
        &self.windows
    }

    #[must_use]
    pub fn workspaces(&self) -> &HashMap<String, WorkspaceState> {
        &self.workspaces
    }

    #[must_use]
    pub fn focused_window(&self) -> Option<&str> {
        self.focused_window.as_deref()
    }

    #[must_use]
    pub fn focused_count(&self) -> usize {
        usize::from(self.focused_window.is_some())
    }

    #[must_use]
    pub fn visible_count(&self) -> usize {
        self.windows
            .values()
            .filter(|window| window.attention == AttentionState::Visible)
            .count()
    }

    #[must_use]
    pub fn hidden_count(&self) -> usize {
        self.windows
            .values()
            .filter(|window| window.attention == AttentionState::Hidden)
            .count()
    }

    pub(crate) fn upsert_workspace(&mut self, id: i64, name: &str, monitor: Option<String>) {
        let entry = self
            .workspaces
            .entry(name.to_owned())
            .or_insert_with(|| WorkspaceState {
                id,
                name: name.to_owned(),
                monitor: monitor.clone(),
                active: false,
                window_count: 0,
            });
        entry.id = id;
        if monitor.is_some() {
            entry.monitor = monitor;
        }
        self.refresh();
    }

    pub(crate) fn remove_workspace(&mut self, name: &str) {
        self.workspaces.remove(name);
        self.active_workspaces
            .retain(|_, workspace| workspace != name);
        self.refresh();
    }

    pub(crate) fn rename_workspace(&mut self, id: i64, new_name: &str) {
        let old_name = self
            .workspaces
            .iter()
            .find_map(|(name, workspace)| (workspace.id == id).then(|| name.clone()));

        let Some(old_name) = old_name else {
            return;
        };

        if let Some(mut workspace) = self.workspaces.remove(&old_name) {
            new_name.clone_into(&mut workspace.name);
            self.workspaces.insert(new_name.to_owned(), workspace);
        }

        for window in self.windows.values_mut() {
            if window.workspace == old_name {
                new_name.clone_into(&mut window.workspace);
            }
        }

        for workspace in self.active_workspaces.values_mut() {
            if *workspace == old_name {
                new_name.clone_into(workspace);
            }
        }

        self.refresh();
    }

    pub(crate) fn upsert_window(&mut self, mut window: WindowState) {
        window.attention = AttentionState::Hidden;
        self.windows.insert(window.address.clone(), window);
        self.refresh();
    }

    pub(crate) fn remove_window(&mut self, address: &str) {
        self.windows.remove(address);
        if self.focused_window.as_deref() == Some(address) {
            self.focused_window = None;
        }
        self.refresh();
    }

    pub(crate) fn move_window(&mut self, address: &str, workspace: String) {
        if let Some(window) = self.windows.get_mut(address) {
            window.workspace = workspace;
        }
        self.refresh();
    }

    pub(crate) fn set_window_title(&mut self, address: &str, title: String) {
        if let Some(window) = self.windows.get_mut(address) {
            window.title = title;
        }
    }

    pub(crate) fn set_focused_window(&mut self, address: Option<String>) {
        self.focused_window = address.filter(|address| self.windows.contains_key(address));
        self.refresh();
    }

    pub(crate) fn set_active_workspace(&mut self, monitor: String, workspace: String) {
        self.active_workspaces.insert(monitor, workspace);
        self.refresh();
    }

    pub(crate) fn set_fullscreen(&mut self, fullscreen: bool) {
        let Some(address) = self.focused_window.clone() else {
            return;
        };
        if let Some(window) = self.windows.get_mut(&address) {
            window.fullscreen = fullscreen;
        }
    }

    fn refresh(&mut self) {
        for workspace in self.workspaces.values_mut() {
            workspace.window_count = 0;
            workspace.active = self
                .active_workspaces
                .values()
                .any(|active| active == &workspace.name);
        }

        for window in self.windows.values_mut() {
            if let Some(workspace) = self.workspaces.get_mut(&window.workspace) {
                workspace.window_count += 1;
                if window.monitor.is_none() {
                    window.monitor.clone_from(&workspace.monitor);
                }
            }

            window.attention = if self.focused_window.as_deref() == Some(&window.address) {
                AttentionState::Focused
            } else if window.mapped
                && !window.hidden
                && self
                    .active_workspaces
                    .values()
                    .any(|active| active == &window.workspace)
            {
                AttentionState::Visible
            } else {
                AttentionState::Hidden
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AttentionGraph, AttentionState, WindowState};

    fn window(address: &str, workspace: &str) -> WindowState {
        WindowState {
            address: address.to_owned(),
            workspace: workspace.to_owned(),
            class: "kitty".to_owned(),
            title: "shell".to_owned(),
            monitor: Some("DP-1".to_owned()),
            mapped: true,
            hidden: false,
            fullscreen: false,
            attention: AttentionState::Hidden,
        }
    }

    #[test]
    fn active_workspace_windows_are_visible() {
        let mut graph = AttentionGraph::default();
        graph.upsert_workspace(1, "dev", Some("DP-1".to_owned()));
        graph.upsert_window(window("0x1", "dev"));
        graph.set_active_workspace("DP-1".to_owned(), "dev".to_owned());

        assert_eq!(graph.windows()["0x1"].attention, AttentionState::Visible);
    }

    #[test]
    fn focused_window_overrides_visibility() {
        let mut graph = AttentionGraph::default();
        graph.upsert_workspace(1, "dev", Some("DP-1".to_owned()));
        graph.upsert_window(window("0x1", "dev"));
        graph.set_active_workspace("DP-1".to_owned(), "dev".to_owned());
        graph.set_focused_window(Some("0x1".to_owned()));

        assert_eq!(graph.windows()["0x1"].attention, AttentionState::Focused);
    }
}
