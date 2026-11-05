//! Process plug-ins' UI drawn natively: the widget trees of their panels and dialogs
//! ([`photocraft_process_plugin_protocol::widget`]), kept here and drawn every frame with
//! PhotoCraft's own widgets and theme.
//!
//! - [`PluginUi`] holds each panel's and dialog's [`Tree`], applies the plug-in's full trees and
//!   patches, and caches the images plug-ins send once by hash (`image.put`).
//! - [`render`] draws a tree and turns what the user does into input events.
//!
//! A plug-in is never waited on: what the user changes shows at once (the tree's local values),
//! and the plug-in's next patch of that widget confirms or corrects it.

mod images;
pub mod render;
#[cfg(test)]
mod tests;

use std::collections::HashMap;

use photocraft_process_plugin_protocol::widget::{Node, Patch};
use serde_json::Value;

pub use images::ImageCache;

/// A panel's or dialog's widget tree and the values the user changed that the plug-in hasn't
/// confirmed yet.
#[derive(Debug, Clone)]
pub struct Tree {
    pub root: Node,
    /// Values shown in place of the props until the plug-in sets that widget again, keyed by
    /// node id and prop name (`value`, `text`, `checked`, `current`, `selected`, `expanded`).
    pub(crate) local: HashMap<(String, &'static str), Value>,
}

impl Tree {
    /// A tree from the plug-in, checked against the protocol's limits.
    pub fn new(root: Node) -> Result<Self, String> {
        root.validate()?;
        Ok(Tree { root, local: HashMap::new() })
    }

    /// Apply one patch from the plug-in. A failed patch leaves the tree as it was.
    pub fn apply(&mut self, patch: Patch) -> Result<(), String> {
        let mut root = self.root.clone();
        match patch {
            Patch::Replace { node } => {
                root = node;
                self.local.clear();
            }
            Patch::Set { id, props } => {
                let node = find_mut(&mut root, &id).ok_or_else(|| format!("no widget '{id}'"))?;
                for (key, value) in props {
                    // The plug-in has spoken for this prop: the user's unconfirmed value gives way.
                    self.local.retain(|(node_id, prop), _| !(node_id == &id && key == *prop));
                    if value.is_null() {
                        node.props.remove(&key);
                    } else {
                        node.props.insert(key, value);
                    }
                }
            }
            Patch::Insert { parent, index, node } => {
                let parent = find_mut(&mut root, &parent).ok_or_else(|| format!("no widget '{parent}'"))?;
                let at = index.min(parent.children.len());
                parent.children.insert(at, node);
            }
            Patch::Remove { id } => {
                if root.id == id {
                    return Err("the root can't be removed".into());
                }
                take(&mut root, &id).ok_or_else(|| format!("no widget '{id}'"))?;
            }
            Patch::Move { id, parent, index } => {
                if root.id == id {
                    return Err("the root can't be moved".into());
                }
                let node = take(&mut root, &id).ok_or_else(|| format!("no widget '{id}'"))?;
                // A node can't move into its own subtree: it was taken out, so its descendants
                // aren't found any more.
                let target = find_mut(&mut root, &parent).ok_or_else(|| format!("no widget '{parent}' (or it is inside '{id}')"))?;
                let at = index.min(target.children.len());
                target.children.insert(at, node);
            }
        }
        root.validate()?;
        self.root = root;
        Ok(())
    }

    /// The node with id `id`, if any.
    pub fn find(&self, id: &str) -> Option<&Node> {
        let mut stack = vec![&self.root];
        while let Some(node) = stack.pop() {
            if node.id == id {
                return Some(node);
            }
            stack.extend(node.children.iter());
        }
        None
    }
}

fn find_mut<'a>(node: &'a mut Node, id: &str) -> Option<&'a mut Node> {
    if node.id == id {
        return Some(node);
    }
    node.children.iter_mut().find_map(|child| find_mut(child, id))
}

/// Take the node `id` out of the tree under `node`.
fn take(node: &mut Node, id: &str) -> Option<Node> {
    if let Some(at) = node.children.iter().position(|c| c.id == id) {
        return Some(node.children.remove(at));
    }
    node.children.iter_mut().find_map(|child| take(child, id))
}

/// Who a tree belongs to: the plug-in and its panel or dialog id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Owner {
    pub plugin: String,
    pub id: String,
}

/// Every plug-in panel and dialog tree, and the plug-ins' images.
#[derive(Default)]
pub struct PluginUi {
    pub trees: HashMap<Owner, Tree>,
    pub images: ImageCache,
    /// Registered panels and how their input reaches the plug-in (`process_plugin_panels`).
    pub panels: crate::process_plugin_panels::Panels,
    /// Open dialogs and waiting message boxes (`process_plugin_dialogs`).
    pub dialogs: crate::process_plugin_dialogs::Dialogs,
    /// Registered actions by command id (`process_plugin_actions`).
    pub actions: std::collections::BTreeMap<String, crate::process_plugin_actions::Action>,
    /// Packages plug-ins asked to be updated to (`plugin.update`), as (plug-in, path): applied
    /// after their answers are sent, since updating stops the plug-in.
    pub updates: Vec<(String, String)>,
}

impl PluginUi {
    /// Forget everything `plugin` showed (it stopped).
    pub fn drop_plugin(&mut self, plugin: &str) {
        self.trees.retain(|owner, _| owner.plugin != plugin);
        self.images.drop_plugin(plugin);
    }
}
