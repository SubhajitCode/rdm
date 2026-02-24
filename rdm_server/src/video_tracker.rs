use crate::types::VideoListItem;
use log::{error, info};
use std::collections::HashMap;

pub struct VideoTracker {
    videos: HashMap<String, VideoListItem>,
}

impl VideoTracker {
    pub fn new() -> Self {
        Self {
            videos: HashMap::new(),
        }
    }

    pub fn add_or_update(&mut self, item: VideoListItem) {
        self.videos.insert(item.id.clone(), item);
    }

    /// Look up a video by `id` and return a clone of its data.
    /// The caller is responsible for dispatching the actual download.
    pub fn trigger_download(&self, id: &str) -> Result<VideoListItem, String> {
        match self.videos.get(id) {
            Some(item) => {
                info!("VideoTracker::trigger_download: id={}", item.id);
                Ok(item.clone())
            }
            None => {
                error!("VideoTracker::trigger_download: video id {} not found", id);
                Err(format!("video id {} not found", id))
            }
        }
    }

    pub fn clear(&mut self) {
        self.videos.clear();
    }

    pub fn get_list(&self) -> Vec<VideoListItem> {
        self.videos.values().cloned().collect()
    }

    pub fn remove(&mut self, id: &str) -> Option<VideoListItem> {
        self.videos.remove(id)
    }

    /// Update the `text` (title) of any video whose `tab_id` matches the
    /// given tab URL.  Called when the extension reports a tab-title change.
    pub fn update_title_for_tab(&mut self, tab_url: &str, new_title: &str) {
        for item in self.videos.values_mut() {
            if item.tab_id == tab_url {
                item.text = new_title.to_string();
                info!(
                    "VideoTracker::update_title_for_tab: id={} new_title={}",
                    item.id, new_title
                );
            }
        }
    }
}
