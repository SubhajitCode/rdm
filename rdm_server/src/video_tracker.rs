use crate::types::VideoListItem;
use log::{error, info};
use std::collections::HashMap;
use url::Url;

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
        let canonical_id = canonical_video_id(&item);
        let mut item = item;
        item.id = canonical_id.clone();

        match self.videos.get_mut(&canonical_id) {
            Some(existing) => merge_video_item(existing, item),
            None => {
                self.videos.insert(canonical_id, item);
            }
        }
    }

    /// Look up a video by `id` and return a clone of its data.
    /// The caller is responsible for dispatching the actual download.
    pub fn get_video(&self, id: &str) -> Result<VideoListItem, String> {
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

    /// Update the `text` (title) of any video whose tracked tab URL matches the
    /// given tab URL. Called when the extension reports a tab-title change.
    pub fn update_title_for_tab(&mut self, tab_url: &str, new_title: &str) {
        for item in self.videos.values_mut() {
            if item.tab_url.as_deref() == Some(tab_url) {
                item.text = new_title.to_string();
                info!(
                    "VideoTracker::update_title_for_tab: id={} new_title={}",
                    item.id, new_title
                );
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum CaptureRank {
    Segment = 0,
    File = 1,
    Manifest = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamFamily {
    Hls,
    Dash,
    Generic,
}

fn merge_video_item(existing: &mut VideoListItem, incoming: VideoListItem) {
    let existing_rank = capture_rank(existing);
    let incoming_rank = capture_rank(&incoming);
    if incoming_rank >= existing_rank {
        existing.url = incoming.url.clone();
        existing.cookie = incoming.cookie.clone();
        existing.request_headers = incoming.request_headers.clone();
        existing.response_headers = incoming.response_headers.clone();
        existing.method = incoming.method.clone();
        existing.user_agent = incoming.user_agent.clone();
        existing.referer = incoming.referer.clone();
        if !incoming.info.is_empty() {
            existing.info = incoming.info.clone();
        }
    }

    if is_better_text(&incoming.text, &existing.text) {
        existing.text = incoming.text;
    }
    if existing.tab_id.is_empty() && !incoming.tab_id.is_empty() {
        existing.tab_id = incoming.tab_id;
    }
    if existing.tab_url.is_none() && incoming.tab_url.is_some() {
        existing.tab_url = incoming.tab_url;
    }
    if existing.referer.is_none() && incoming.referer.is_some() {
        existing.referer = incoming.referer;
    }
    if existing.info.is_empty() && !incoming.info.is_empty() {
        existing.info = incoming.info;
    }
}

fn canonical_video_id(item: &VideoListItem) -> String {
    let family = stream_family(item);
    if matches!(family, StreamFamily::Hls | StreamFamily::Dash) {
        if let Some(scope) = stream_scope(item) {
            return stable_hash(&format!("stream::{family:?}::{scope}"));
        }
    }
    stable_hash(&format!("url::{}", item.url))
}

fn stream_scope(item: &VideoListItem) -> Option<String> {
    let tab_scope = if !item.tab_id.is_empty() {
        Some(format!("tab:{}", item.tab_id))
    } else {
        item.tab_url
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(|value| format!("taburl:{}", normalized_origin_and_path(value)))
    }
    .or_else(|| {
        item.referer
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(|value| format!("ref:{}", normalized_origin_and_path(value)))
    });

    tab_scope.map(|scope| {
        let media_host = normalized_host(&item.url).unwrap_or_default();
        format!("{scope}::{media_host}")
    })
}

fn stream_family(item: &VideoListItem) -> StreamFamily {
    let path = item.url.split('?').next().unwrap_or(&item.url).to_ascii_lowercase();
    let mime = item
        .info
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    if path.ends_with(".m3u8")
        || path.ends_with(".ts")
        || path.ends_with(".aac")
        || mime == "application/vnd.apple.mpegurl"
        || mime == "application/x-mpegurl"
        || mime == "video/mp2t"
    {
        StreamFamily::Hls
    } else if path.ends_with(".mpd") || path.ends_with(".m4s") || mime == "application/dash+xml" {
        StreamFamily::Dash
    } else {
        StreamFamily::Generic
    }
}

fn capture_rank(item: &VideoListItem) -> CaptureRank {
    let path = item.url.split('?').next().unwrap_or(&item.url).to_ascii_lowercase();
    let mime = item
        .info
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    if path.ends_with(".m3u8")
        || path.ends_with(".mpd")
        || mime == "application/vnd.apple.mpegurl"
        || mime == "application/x-mpegurl"
        || mime == "application/dash+xml"
    {
        CaptureRank::Manifest
    } else if path.ends_with(".ts") || path.ends_with(".m4s") || mime == "video/mp2t" {
        CaptureRank::Segment
    } else {
        CaptureRank::File
    }
}

fn is_better_text(incoming: &str, current: &str) -> bool {
    let incoming = incoming.trim();
    let current = current.trim();
    !incoming.is_empty()
        && (current.is_empty() || current.starts_with("http://") || current.starts_with("https://"))
}

fn normalized_host(value: &str) -> Option<String> {
    Url::parse(value)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_ascii_lowercase()))
}

fn normalized_origin_and_path(value: &str) -> String {
    match Url::parse(value) {
        Ok(url) => {
            let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
            let mut path = url.path().trim_end_matches('/').to_string();
            if path.is_empty() {
                path.push('/');
            }
            format!("{}://{}{}", url.scheme(), host, path)
        }
        Err(_) => value.to_string(),
    }
}

fn stable_hash(value: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::VideoTracker;
    use crate::types::VideoListItem;
    use std::collections::HashMap;

    fn item(url: &str, info: &str, tab_id: &str, tab_url: &str, title: &str) -> VideoListItem {
        VideoListItem {
            id: "raw".to_string(),
            text: title.to_string(),
            info: info.to_string(),
            tab_id: tab_id.to_string(),
            url: url.to_string(),
            cookie: String::new(),
            request_headers: HashMap::new(),
            response_headers: HashMap::new(),
            method: Some("GET".to_string()),
            user_agent: None,
            tab_url: Some(tab_url.to_string()),
            referer: Some(tab_url.to_string()),
        }
    }

    #[test]
    fn merges_hls_segments_into_single_manifest_entry() {
        let mut tracker = VideoTracker::new();
        tracker.add_or_update(item(
            "https://cdn.example.com/video/seg-1.ts",
            "video/mp2t",
            "17",
            "https://app.example.com/watch/abc",
            "Example stream",
        ));
        tracker.add_or_update(item(
            "https://cdn.example.com/video/master.m3u8",
            "application/vnd.apple.mpegurl",
            "17",
            "https://app.example.com/watch/abc",
            "Example stream",
        ));

        let list = tracker.get_list();
        assert_eq!(list.len(), 1);
        assert!(list[0].url.ends_with("master.m3u8"));
    }

    #[test]
    fn merges_dash_segments_into_single_manifest_entry() {
        let mut tracker = VideoTracker::new();
        tracker.add_or_update(item(
            "https://cdn.example.com/video/chunk-1.m4s",
            "video/iso.segment",
            "7",
            "https://app.example.com/watch/xyz",
            "Dash stream",
        ));
        tracker.add_or_update(item(
            "https://cdn.example.com/video/manifest.mpd",
            "application/dash+xml",
            "7",
            "https://app.example.com/watch/xyz",
            "Dash stream",
        ));

        let list = tracker.get_list();
        assert_eq!(list.len(), 1);
        assert!(list[0].url.ends_with("manifest.mpd"));
    }

    #[test]
    fn keeps_direct_files_separate() {
        let mut tracker = VideoTracker::new();
        tracker.add_or_update(item(
            "https://cdn.example.com/video/file-1.mp4",
            "video/mp4",
            "",
            "https://app.example.com/watch/a",
            "First",
        ));
        tracker.add_or_update(item(
            "https://cdn.example.com/video/file-2.mp4",
            "video/mp4",
            "",
            "https://app.example.com/watch/b",
            "Second",
        ));

        assert_eq!(tracker.get_list().len(), 2);
    }
}
