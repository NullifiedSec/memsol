use std::{
    collections::HashMap,
    env,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
};

use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::attention::{AttentionGraph, AttentionState, WindowState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HyprlandPaths {
    pub request_socket: PathBuf,
    pub event_socket: PathBuf,
}

impl HyprlandPaths {
    /// Resolves sockets for the current Hyprland session.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::NotFound`] when the required Hyprland session
    /// environment variables are absent.
    pub fn discover() -> io::Result<Self> {
        let runtime_dir = env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set")
        })?;
        let signature = env::var_os("HYPRLAND_INSTANCE_SIGNATURE").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "HYPRLAND_INSTANCE_SIGNATURE is not set",
            )
        })?;

        let base = PathBuf::from(runtime_dir).join("hypr").join(signature);
        Ok(Self {
            request_socket: base.join(".socket.sock"),
            event_socket: base.join(".socket2.sock"),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HyprlandEvent {
    WorkspaceChanged { id: i64, name: String },
    FocusedMonitor { monitor: String, workspace: String },
    ActiveWindow { address: Option<String> },
    WindowOpened {
        address: String,
        workspace: String,
        class: String,
        title: String,
    },
    WindowClosed { address: String },
    WindowMoved { address: String, workspace: String },
    WorkspaceCreated { id: i64, name: String },
    WorkspaceDestroyed { id: i64, name: String },
    WorkspaceRenamed { id: i64, name: String },
    WindowTitle { address: String, title: String },
    Fullscreen(bool),
}

pub struct HyprlandEventStream {
    reader: BufReader<UnixStream>,
}

impl HyprlandEventStream {
    /// Connects to Hyprland's live event socket.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the session cannot be discovered or the event
    /// socket cannot be opened.
    pub fn connect() -> io::Result<Self> {
        let paths = HyprlandPaths::discover()?;
        let stream = UnixStream::connect(paths.event_socket)?;
        Ok(Self {
            reader: BufReader::new(stream),
        })
    }

    /// Waits for and parses the next event memsol currently understands.
    ///
    /// Unknown Hyprland events are deliberately skipped so adding a compositor
    /// event does not break the observer.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when reading from the Hyprland event socket fails.
    pub fn next_event(&mut self) -> io::Result<Option<HyprlandEvent>> {
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line)? == 0 {
                return Ok(None);
            }
            if let Some(event) = parse_event(line.trim_end()) {
                return Ok(Some(event));
            }
        }
    }
}

/// Reads one consistent-enough startup snapshot from Hyprland.
///
/// Long-running observers should connect to the event socket before taking this
/// snapshot, then consume the queued events afterward. This closes the normal
/// snapshot-to-subscription race without high-frequency polling.
///
/// # Errors
///
/// Returns an I/O or JSON error when Hyprland's request socket cannot be read or
/// returns data incompatible with the expected client/monitor/workspace schema.
pub fn snapshot_attention() -> io::Result<AttentionGraph> {
    let paths = HyprlandPaths::discover()?;
    let monitors: Vec<MonitorSnapshot> = request_json(&paths, "j/monitors")?;
    let workspaces: Vec<WorkspaceSnapshot> = request_json(&paths, "j/workspaces")?;
    let clients: Vec<ClientSnapshot> = request_json(&paths, "j/clients")?;
    let active_window: serde_json::Value = request_json(&paths, "j/activewindow")?;

    let monitor_names: HashMap<i64, String> = monitors
        .iter()
        .map(|monitor| (monitor.id, monitor.name.clone()))
        .collect();

    let mut graph = AttentionGraph::default();

    for workspace in workspaces {
        let monitor = (!workspace.monitor.is_empty())
            .then_some(workspace.monitor)
            .or_else(|| monitor_names.get(&workspace.monitor_id).cloned());
        graph.upsert_workspace(workspace.id, workspace.name, monitor);
    }

    for monitor in monitors {
        graph.upsert_workspace(
            monitor.active_workspace.id,
            monitor.active_workspace.name.clone(),
            Some(monitor.name.clone()),
        );
        graph.set_active_workspace(monitor.name, monitor.active_workspace.name);
    }

    for client in clients {
        graph.upsert_workspace(
            client.workspace.id,
            client.workspace.name.clone(),
            monitor_names.get(&client.monitor).cloned(),
        );
        graph.upsert_window(WindowState {
            address: normalize_address(&client.address),
            workspace: client.workspace.name,
            class: client.class,
            title: client.title,
            monitor: monitor_names.get(&client.monitor).cloned(),
            mapped: client.mapped,
            hidden: client.hidden,
            fullscreen: client.fullscreen != 0,
            attention: AttentionState::Hidden,
        });
    }

    let focused = active_window
        .get("address")
        .and_then(serde_json::Value::as_str)
        .filter(|address| !address.is_empty())
        .map(normalize_address);
    graph.set_focused_window(focused);

    Ok(graph)
}

pub fn apply_event(graph: &mut AttentionGraph, event: HyprlandEvent) {
    match event {
        HyprlandEvent::WorkspaceChanged { id, name }
        | HyprlandEvent::WorkspaceCreated { id, name } => {
            graph.upsert_workspace(id, name, None);
        }
        HyprlandEvent::FocusedMonitor { monitor, workspace } => {
            graph.set_active_workspace(monitor, workspace);
        }
        HyprlandEvent::ActiveWindow { address } => graph.set_focused_window(address),
        HyprlandEvent::WindowOpened {
            address,
            workspace,
            class,
            title,
        } => {
            graph.upsert_window(WindowState {
                address,
                workspace,
                class,
                title,
                monitor: None,
                mapped: true,
                hidden: false,
                fullscreen: false,
                attention: AttentionState::Hidden,
            });
        }
        HyprlandEvent::WindowClosed { address } => graph.remove_window(&address),
        HyprlandEvent::WindowMoved { address, workspace } => graph.move_window(&address, workspace),
        HyprlandEvent::WorkspaceDestroyed { id: _, name } => graph.remove_workspace(&name),
        HyprlandEvent::WorkspaceRenamed { id, name } => graph.rename_workspace(id, name),
        HyprlandEvent::WindowTitle { address, title } => graph.set_window_title(&address, title),
        HyprlandEvent::Fullscreen(fullscreen) => graph.set_fullscreen(fullscreen),
    }
}

#[must_use]
pub fn parse_event(line: &str) -> Option<HyprlandEvent> {
    let (name, data) = line.split_once(">>")?;
    match name {
        "workspacev2" => {
            let (id, workspace) = data.split_once(',')?;
            Some(HyprlandEvent::WorkspaceChanged {
                id: id.parse().ok()?,
                name: workspace.to_owned(),
            })
        }
        "focusedmon" => {
            let (monitor, workspace) = data.split_once(',')?;
            Some(HyprlandEvent::FocusedMonitor {
                monitor: monitor.to_owned(),
                workspace: workspace.to_owned(),
            })
        }
        "activewindowv2" => Some(HyprlandEvent::ActiveWindow {
            address: (!data.is_empty()).then(|| normalize_address(data)),
        }),
        "openwindow" => {
            let mut fields = data.splitn(4, ',');
            Some(HyprlandEvent::WindowOpened {
                address: normalize_address(fields.next()?),
                workspace: fields.next()?.to_owned(),
                class: fields.next()?.to_owned(),
                title: fields.next().unwrap_or_default().to_owned(),
            })
        }
        "closewindow" => Some(HyprlandEvent::WindowClosed {
            address: normalize_address(data),
        }),
        "movewindowv2" => {
            let mut fields = data.splitn(3, ',');
            Some(HyprlandEvent::WindowMoved {
                address: normalize_address(fields.next()?),
                workspace: {
                    let _id = fields.next()?;
                    fields.next()?.to_owned()
                },
            })
        }
        "createworkspacev2" => parse_workspace_event(data, true),
        "destroyworkspacev2" => parse_workspace_event(data, false),
        "renameworkspace" => {
            let (id, workspace) = data.split_once(',')?;
            Some(HyprlandEvent::WorkspaceRenamed {
                id: id.parse().ok()?,
                name: workspace.to_owned(),
            })
        }
        "windowtitlev2" => {
            let (address, title) = data.split_once(',')?;
            Some(HyprlandEvent::WindowTitle {
                address: normalize_address(address),
                title: title.to_owned(),
            })
        }
        "fullscreen" => match data {
            "0" => Some(HyprlandEvent::Fullscreen(false)),
            "1" => Some(HyprlandEvent::Fullscreen(true)),
            _ => None,
        },
        _ => None,
    }
}

fn parse_workspace_event(data: &str, created: bool) -> Option<HyprlandEvent> {
    let (id, name) = data.split_once(',')?;
    let id = id.parse().ok()?;
    if created {
        Some(HyprlandEvent::WorkspaceCreated {
            id,
            name: name.to_owned(),
        })
    } else {
        Some(HyprlandEvent::WorkspaceDestroyed {
            id,
            name: name.to_owned(),
        })
    }
}

fn normalize_address(address: &str) -> String {
    if address.is_empty() || address.starts_with("0x") {
        address.to_owned()
    } else {
        format!("0x{address}")
    }
}

fn request_json<T: DeserializeOwned>(paths: &HyprlandPaths, command: &str) -> io::Result<T> {
    let mut stream = UnixStream::connect(&paths.request_socket)?;
    stream.write_all(command.as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    serde_json::from_str(&response).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[derive(Debug, Deserialize)]
struct WorkspaceRef {
    id: i64,
    name: String,
}

#[derive(Debug, Deserialize)]
struct MonitorSnapshot {
    id: i64,
    name: String,
    #[serde(rename = "activeWorkspace")]
    active_workspace: WorkspaceRef,
}

#[derive(Debug, Deserialize)]
struct WorkspaceSnapshot {
    id: i64,
    name: String,
    #[serde(default)]
    monitor: String,
    #[serde(rename = "monitorID", default)]
    monitor_id: i64,
}

#[derive(Debug, Deserialize)]
struct ClientSnapshot {
    address: String,
    #[serde(default)]
    mapped: bool,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    class: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    monitor: i64,
    workspace: WorkspaceRef,
    #[serde(default)]
    fullscreen: i64,
}

#[cfg(test)]
mod tests {
    use super::{HyprlandEvent, parse_event};

    #[test]
    fn parses_open_window_titles_with_commas() {
        let event = parse_event("openwindow>>abc,dev,firefox,Docs, API reference")
            .expect("valid openwindow event");

        assert_eq!(
            event,
            HyprlandEvent::WindowOpened {
                address: "0xabc".to_owned(),
                workspace: "dev".to_owned(),
                class: "firefox".to_owned(),
                title: "Docs, API reference".to_owned(),
            }
        );
    }

    #[test]
    fn parses_empty_active_window_as_no_focus() {
        assert_eq!(
            parse_event("activewindowv2>>"),
            Some(HyprlandEvent::ActiveWindow { address: None })
        );
    }

    #[test]
    fn parses_window_move() {
        assert_eq!(
            parse_event("movewindowv2>>abc,4,name:docs"),
            Some(HyprlandEvent::WindowMoved {
                address: "0xabc".to_owned(),
                workspace: "name:docs".to_owned(),
            })
        );
    }
}
