//! Runtime application state shared between the UI and the engine threads,
//! plus scene/source list manipulation used by the Scenes and Sources tab.

use crate::config::{BusConfig, Config, SceneConfig, ScenesConfig};

/// Result of a list-manipulation attempt, so the UI knows whether anything
/// changed (and therefore whether to refresh and re-persist).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListEdit {
    Changed,
    Unchanged,
}

/// Moves the item at `index` up (towards 0) if possible.
pub fn move_up<T>(items: &mut [T], index: usize) -> ListEdit {
    if index == 0 || index >= items.len() {
        return ListEdit::Unchanged;
    }
    items.swap(index, index - 1);
    ListEdit::Changed
}

/// Moves the item at `index` down (towards the end) if possible.
pub fn move_down<T>(items: &mut [T], index: usize) -> ListEdit {
    if items.len() < 2 || index >= items.len() - 1 {
        return ListEdit::Unchanged;
    }
    items.swap(index, index + 1);
    ListEdit::Changed
}

impl ScenesConfig {
    pub fn active_scene(&self) -> Option<&SceneConfig> {
        self.scenes.iter().find(|s| s.name == self.active_scene)
    }

    pub fn active_scene_mut(&mut self) -> Option<&mut SceneConfig> {
        self.scenes.iter_mut().find(|s| s.name == self.active_scene)
    }

    /// Switches the active scene. Returns `Unchanged` when the target is
    /// already active or doesn't exist (the spec requires switching to the
    /// active scene to be a no-op).
    pub fn switch_to(&mut self, name: &str) -> ListEdit {
        if self.active_scene == name || !self.scenes.iter().any(|s| s.name == name) {
            return ListEdit::Unchanged;
        }
        self.active_scene = name.to_string();
        ListEdit::Changed
    }

    /// Deletes the scene at `index` unless it is the permanent default scene.
    pub fn delete_scene(&mut self, index: usize) -> ListEdit {
        let Some(scene) = self.scenes.get(index) else {
            return ListEdit::Unchanged;
        };
        if scene.is_default {
            return ListEdit::Unchanged;
        }
        let removed = self.scenes.remove(index);
        if self.active_scene == removed.name {
            self.ensure_default_scene();
        }
        ListEdit::Changed
    }

    /// Adds a scene, rejecting empty or duplicate names.
    pub fn add_scene(&mut self, name: &str) -> ListEdit {
        let name = name.trim();
        if name.is_empty() || self.scenes.iter().any(|s| s.name == name) {
            return ListEdit::Unchanged;
        }
        self.scenes.push(SceneConfig {
            name: name.to_string(),
            is_default: false,
            sources: Vec::new(),
        });
        ListEdit::Changed
    }

    /// Renames the scene at `index` (allowed for the default scene too).
    pub fn rename_scene(&mut self, index: usize, new_name: &str) -> ListEdit {
        let new_name = new_name.trim();
        if new_name.is_empty()
            || self
                .scenes
                .iter()
                .enumerate()
                .any(|(i, s)| i != index && s.name == new_name)
        {
            return ListEdit::Unchanged;
        }
        let Some(scene) = self.scenes.get_mut(index) else {
            return ListEdit::Unchanged;
        };
        if scene.name == new_name {
            return ListEdit::Unchanged;
        }
        let old_name = std::mem::replace(&mut scene.name, new_name.to_string());
        if self.active_scene == old_name {
            self.active_scene = new_name.to_string();
        }
        ListEdit::Changed
    }
}

/// Bus list edits live on `Config` (not `BusesConfig`) because renaming or
/// deleting a bus must also rewrite the sends that reference it by name,
/// and those live inside the scenes.
impl Config {
    /// Adds a bus, rejecting empty or duplicate names.
    pub fn add_bus(&mut self, name: &str) -> ListEdit {
        let name = name.trim();
        if name.is_empty() || self.buses.buses.iter().any(|b| b.name == name) {
            return ListEdit::Unchanged;
        }
        self.buses.buses.push(BusConfig {
            name: name.to_string(),
            ..Default::default()
        });
        ListEdit::Changed
    }

    /// Renames the bus at `index`, updating every send that targets it.
    pub fn rename_bus(&mut self, index: usize, new_name: &str) -> ListEdit {
        let new_name = new_name.trim();
        if new_name.is_empty()
            || self
                .buses
                .buses
                .iter()
                .enumerate()
                .any(|(i, b)| i != index && b.name == new_name)
        {
            return ListEdit::Unchanged;
        }
        let Some(bus) = self.buses.buses.get_mut(index) else {
            return ListEdit::Unchanged;
        };
        if bus.name == new_name {
            return ListEdit::Unchanged;
        }
        let old_name = std::mem::replace(&mut bus.name, new_name.to_string());
        for scene in &mut self.scenes.scenes {
            for source in &mut scene.sources {
                for send in &mut source.sends {
                    if send.bus == old_name {
                        send.bus = new_name.to_string();
                    }
                }
            }
        }
        ListEdit::Changed
    }

    /// Deletes the bus at `index` and strips every send that targeted it.
    pub fn delete_bus(&mut self, index: usize) -> ListEdit {
        if index >= self.buses.buses.len() {
            return ListEdit::Unchanged;
        }
        let removed = self.buses.buses.remove(index);
        for scene in &mut self.scenes.scenes {
            for source in &mut scene.sources {
                source.sends.retain(|s| s.bus != removed.name);
            }
        }
        ListEdit::Changed
    }
}

/// Formats a message age per the spec: "Just now" for the first few seconds,
/// then seconds up to a minute, minutes up to an hour, then hours, then days —
/// unit always after the number.
pub fn relative_time(age: std::time::Duration) -> String {
    let secs = age.as_secs();
    fn plural(n: u64, unit: &str) -> String {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    }
    match secs {
        0..=4 => "Just now".to_string(),
        5..=59 => plural(secs, "second"),
        60..=3599 => plural(secs / 60, "minute"),
        3600..=86399 => plural(secs / 3600, "hour"),
        _ => plural(secs / 86400, "day"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SceneConfig;
    use std::time::Duration;

    fn scenes_with(names: &[(&str, bool)]) -> ScenesConfig {
        ScenesConfig {
            scenes: names
                .iter()
                .map(|(n, d)| SceneConfig {
                    name: n.to_string(),
                    is_default: *d,
                    sources: Vec::new(),
                })
                .collect(),
            active_scene: names[0].0.to_string(),
        }
    }

    #[test]
    fn relative_time_formatting() {
        assert_eq!(relative_time(Duration::from_secs(0)), "Just now");
        assert_eq!(relative_time(Duration::from_secs(3)), "Just now");
        assert_eq!(relative_time(Duration::from_secs(5)), "5 seconds ago");
        assert_eq!(relative_time(Duration::from_secs(59)), "59 seconds ago");
        assert_eq!(relative_time(Duration::from_secs(60)), "1 minute ago");
        assert_eq!(relative_time(Duration::from_secs(185)), "3 minutes ago");
        assert_eq!(relative_time(Duration::from_secs(3600)), "1 hour ago");
        assert_eq!(relative_time(Duration::from_secs(7300)), "2 hours ago");
        assert_eq!(relative_time(Duration::from_secs(200_000)), "2 days ago");
    }

    #[test]
    fn move_up_down() {
        let mut v = vec![1, 2, 3];
        assert_eq!(move_up(&mut v, 0), ListEdit::Unchanged);
        assert_eq!(move_up(&mut v, 2), ListEdit::Changed);
        assert_eq!(v, vec![1, 3, 2]);
        assert_eq!(move_down(&mut v, 2), ListEdit::Unchanged);
        assert_eq!(move_down(&mut v, 0), ListEdit::Changed);
        assert_eq!(v, vec![3, 1, 2]);
    }

    #[test]
    fn default_scene_cannot_be_deleted() {
        let mut s = scenes_with(&[("Default", true), ("Music", false)]);
        assert_eq!(s.delete_scene(0), ListEdit::Unchanged);
        assert_eq!(s.delete_scene(1), ListEdit::Changed);
        assert_eq!(s.scenes.len(), 1);
    }

    #[test]
    fn deleting_active_scene_falls_back_to_default() {
        let mut s = scenes_with(&[("Default", true), ("Music", false)]);
        s.active_scene = "Music".into();
        s.delete_scene(1);
        assert_eq!(s.active_scene, "Default");
    }

    #[test]
    fn switch_to_active_scene_is_noop() {
        let mut s = scenes_with(&[("Default", true), ("Music", false)]);
        assert_eq!(s.switch_to("Default"), ListEdit::Unchanged);
        assert_eq!(s.switch_to("Music"), ListEdit::Changed);
        assert_eq!(s.switch_to("Nope"), ListEdit::Unchanged);
    }

    #[test]
    fn rename_updates_active_and_rejects_duplicates() {
        let mut s = scenes_with(&[("Default", true), ("Music", false)]);
        assert_eq!(s.rename_scene(0, "Main"), ListEdit::Changed);
        assert_eq!(s.active_scene, "Main");
        assert_eq!(s.rename_scene(1, "Main"), ListEdit::Unchanged);
        assert_eq!(s.rename_scene(1, "  "), ListEdit::Unchanged);
    }

    #[test]
    fn add_scene_rejects_duplicates_and_blank() {
        let mut s = scenes_with(&[("Default", true)]);
        assert_eq!(s.add_scene("Music"), ListEdit::Changed);
        assert_eq!(s.add_scene("Music"), ListEdit::Unchanged);
        assert_eq!(s.add_scene("   "), ListEdit::Unchanged);
    }

    fn config_with_bus_and_send() -> Config {
        let mut config = Config::default();
        config.add_bus("Voice FX");
        config.scenes.scenes[0].sources[0]
            .sends
            .push(crate::config::SendConfig {
                bus: "Voice FX".into(),
                level: 80,
            });
        config
    }

    #[test]
    fn add_bus_rejects_duplicates_and_blank() {
        let mut config = Config::default();
        assert_eq!(config.add_bus("Voice FX"), ListEdit::Changed);
        assert_eq!(config.add_bus("Voice FX"), ListEdit::Unchanged);
        assert_eq!(config.add_bus("  "), ListEdit::Unchanged);
        assert_eq!(config.buses.buses.len(), 1);
        assert_eq!(config.buses.buses[0].volume, 100);
    }

    #[test]
    fn rename_bus_rewrites_sends() {
        let mut config = config_with_bus_and_send();
        assert_eq!(config.rename_bus(0, "Vocals"), ListEdit::Changed);
        assert_eq!(config.scenes.scenes[0].sources[0].sends[0].bus, "Vocals");
        // Duplicate and blank rejected.
        config.add_bus("Other");
        assert_eq!(config.rename_bus(1, "Vocals"), ListEdit::Unchanged);
        assert_eq!(config.rename_bus(1, ""), ListEdit::Unchanged);
    }

    #[test]
    fn delete_bus_strips_sends() {
        let mut config = config_with_bus_and_send();
        assert_eq!(config.delete_bus(0), ListEdit::Changed);
        assert!(config.buses.buses.is_empty());
        assert!(config.scenes.scenes[0].sources[0].sends.is_empty());
        assert_eq!(config.delete_bus(0), ListEdit::Unchanged);
    }

    #[test]
    fn fix_up_routing_drops_dangling_sends_and_dup_buses() {
        let mut config = config_with_bus_and_send();
        config.buses.buses.push(crate::config::BusConfig {
            name: "Voice FX".into(),
            ..Default::default()
        });
        config.scenes.scenes[0].sources[1]
            .sends
            .push(crate::config::SendConfig {
                bus: "Ghost".into(),
                level: 50,
            });
        config.fix_up_routing();
        assert_eq!(config.buses.buses.len(), 1);
        assert_eq!(config.scenes.scenes[0].sources[0].sends.len(), 1);
        assert!(config.scenes.scenes[0].sources[1].sends.is_empty());
    }
}
