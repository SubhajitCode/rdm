use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, RwLock as StdRwLock};

use aes::Aes128;
use async_trait::async_trait;
use cbc::cipher::block_padding::{NoPadding, Pkcs7};
use cbc::cipher::{BlockDecryptMut, KeyIvInit};
use dash_mpd::{parse as parse_mpd, AdaptationSet, MPD, Period, Representation};
use m3u8_rs::{parse_playlist_res, KeyMethod, MediaPlaylist, Playlist};
use reqwest::{Client, StatusCode};
use tokio::sync::{mpsc, RwLock, Semaphore};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::downloader::probe_if_needed;
use crate::downloader::strategy::download_strategy::DownloadStrategy;
use crate::downloader::util::{detect_download_kind, ext_from_mime, replace_extension};
use crate::types::types::{
    DownloadError, DownloadKind, DownloadPhase, DownloaderState, ProbeResult, ProgressEvent, Segment,
    SegmentState, StreamType,
};

type Aes128CbcDec = cbc::Decryptor<Aes128>;

#[derive(Clone)]
struct StreamingSegmentJob {
    id: String,
    ordinal: i64,
    url: String,
    byte_range: Option<(u64, u64)>,
    decryption_key: Option<Vec<u8>>,
    decryption_iv: Option<[u8; 16]>,
    stream_type: StreamType,
}

#[derive(Clone)]
struct StreamingPlan {
    jobs: Vec<StreamingSegmentJob>,
    output_extension: String,
    content_type: Option<String>,
}

pub struct StreamingDownloadStrategy {
    state: Arc<StdRwLock<DownloaderState>>,
    segments: Arc<RwLock<HashMap<String, Segment>>>,
    client: Arc<Client>,
    cancel_token: CancellationToken,
    progress_tx: StdMutex<Option<mpsc::Sender<Result<ProgressEvent, String>>>>,
    connections: usize,
    plan: Arc<RwLock<Option<StreamingPlan>>>,
}

impl StreamingDownloadStrategy {
    pub fn from_state(state: DownloaderState, connections: usize) -> Self {
        let client = state.create_client();
        Self {
            state: Arc::new(StdRwLock::new(state)),
            segments: Arc::new(RwLock::new(HashMap::new())),
            client: Arc::new(client),
            cancel_token: CancellationToken::new(),
            progress_tx: StdMutex::new(None),
            connections: connections.max(1),
            plan: Arc::new(RwLock::new(None)),
        }
    }

    pub fn from_probe(mut state: DownloaderState, probe: ProbeResult, connections: usize) -> Self {
        state.file_size = probe.resource_size.map(|size| size as i64).unwrap_or(-1);
        state.url = probe.final_uri;
        state.last_modified = probe.last_modified;
        state.resumable = true;
        state.attachment_name = probe.attachment_name;
        state.content_type = probe.content_type;
        state.download_kind = probe.download_kind;
        Self::from_state(state, connections)
    }

    pub fn from_persisted(state: DownloaderState, segments: Vec<Segment>, connections: usize) -> Self {
        let strategy = Self::from_state(state, connections);
        let mut guard = strategy.segments.blocking_write();
        for segment in segments {
            guard.insert(segment.id.clone(), segment);
        }
        drop(guard);
        strategy
    }

    async fn rebuild_plan(&self) -> Result<StreamingPlan, DownloadError> {
        let (kind, url) = {
            let state = self.state.read().unwrap();
            let kind = if state.download_kind == DownloadKind::Direct {
                detect_download_kind(&state.url, state.content_type.as_deref())
            } else {
                state.download_kind
            };
            (kind, state.url.clone())
        };

        match kind {
            DownloadKind::Hls => self.build_hls_plan(&url).await,
            DownloadKind::Dash => self.build_dash_plan(&url).await,
            DownloadKind::Direct => Err(DownloadError::SegmentFailed(
                "streaming strategy requires an HLS or DASH manifest".to_string(),
            )),
        }
    }

    async fn build_hls_plan(&self, manifest_url: &str) -> Result<StreamingPlan, DownloadError> {
        let playlist = self.fetch_hls_playlist(manifest_url).await?;
        let media = match playlist {
            Playlist::MasterPlaylist(master) => {
                let variant = master
                    .variants
                    .iter()
                    .max_by_key(|variant| variant.bandwidth)
                    .ok_or_else(|| DownloadError::SegmentFailed("HLS master playlist has no variants".into()))?;
                let child_url = resolve_url(manifest_url, &variant.uri)?;
                self.fetch_hls_media_playlist(&child_url).await?
            }
            Playlist::MediaPlaylist(media) => media,
        };

        if !media.end_list {
            return Err(DownloadError::SegmentFailed(
                "live HLS playlists are not supported".to_string(),
            ));
        }

        let mut key_cache: HashMap<String, Vec<u8>> = HashMap::new();
        let mut jobs = Vec::new();
        let mut last_media_range_end: Option<(String, u64)> = None;
        let mut last_init_range_end: Option<(String, u64)> = None;
        let mut next_ordinal = 0_i64;
        let mut last_map_id: Option<String> = None;
        let mut current_key = None;
        let mut current_map = None;

        for (index, segment) in media.segments.iter().enumerate() {
            if let Some(map) = &segment.map {
                current_map = Some(map.clone());
            }
            if let Some(key) = &segment.key {
                current_key = Some(key.clone());
            }

            if let Some(map) = current_map.as_ref() {
                let init_url = resolve_url(manifest_url, &map.uri)?;
                let init_range = map.byte_range.as_ref().map(|range| {
                    resolve_hls_byte_range(&init_url, range.length, range.offset, &mut last_init_range_end)
                }).transpose()?;
                let init_key = format!("{}::{:?}", init_url, init_range);
                if last_map_id.as_deref() != Some(init_key.as_str()) {
                    jobs.push(StreamingSegmentJob {
                        id: format!("init{:05}", jobs.len()),
                        ordinal: next_ordinal,
                        url: init_url.clone(),
                        byte_range: init_range,
                        decryption_key: None,
                        decryption_iv: None,
                        stream_type: StreamType::Primary,
                    });
                    next_ordinal += 1;
                    last_map_id = Some(init_key);
                }
            }

            let segment_url = resolve_url(manifest_url, &segment.uri)?;
            let byte_range = segment.byte_range.as_ref().map(|range| {
                resolve_hls_byte_range(&segment_url, range.length, range.offset, &mut last_media_range_end)
            }).transpose()?;
            let (decryption_key, decryption_iv) =
                self.resolve_hls_encryption(
                    current_key.as_ref(),
                    manifest_url,
                    media.media_sequence + index as u64,
                    &mut key_cache,
                )
                    .await?;

            jobs.push(StreamingSegmentJob {
                id: format!("seg{:05}", index),
                ordinal: next_ordinal,
                url: segment_url,
                byte_range,
                decryption_key,
                decryption_iv,
                stream_type: StreamType::Primary,
            });
            next_ordinal += 1;
        }

        if jobs.is_empty() {
            return Err(DownloadError::SegmentFailed(
                "HLS playlist has no downloadable segments".to_string(),
            ));
        }

        let output_extension = infer_stream_extension(
            DownloadKind::Hls,
            jobs.iter().map(|job| job.url.as_str()),
            None,
        );

        Ok(StreamingPlan {
            jobs,
            output_extension,
            content_type: Some("video/mp2t".to_string()),
        })
    }

    async fn fetch_hls_playlist(&self, url: &str) -> Result<Playlist, DownloadError> {
        let text = self.fetch_text(url).await?;
        parse_playlist_res(text.as_bytes())
            .map(|playlist| playlist)
            .map_err(|err| DownloadError::SegmentFailed(format!("failed to parse HLS manifest: {err:?}")))
    }

    async fn fetch_hls_media_playlist(&self, url: &str) -> Result<MediaPlaylist, DownloadError> {
        match self.fetch_hls_playlist(url).await? {
            Playlist::MediaPlaylist(media) => Ok(media),
            Playlist::MasterPlaylist(_) => Err(DownloadError::SegmentFailed(
                "nested HLS master playlists are not supported".to_string(),
            )),
        }
    }

    async fn resolve_hls_encryption(
        &self,
        key: Option<&m3u8_rs::Key>,
        manifest_url: &str,
        media_sequence: u64,
        key_cache: &mut HashMap<String, Vec<u8>>,
    ) -> Result<(Option<Vec<u8>>, Option<[u8; 16]>), DownloadError> {
        let Some(key) = key else {
            return Ok((None, None));
        };

        if key.method == KeyMethod::None {
            return Ok((None, None));
        }
        if key.method != KeyMethod::AES128 {
            return Err(DownloadError::SegmentFailed(format!(
                "unsupported HLS encryption method: {}",
                key.method
            )));
        }

        let key_uri = key
            .uri
            .as_deref()
            .ok_or_else(|| DownloadError::SegmentFailed("HLS AES-128 key is missing URI".to_string()))?;
        let resolved_key_url = resolve_url(manifest_url, key_uri)?;
        let key_bytes = if let Some(cached) = key_cache.get(&resolved_key_url) {
            cached.clone()
        } else {
            let fetched = self.fetch_bytes(&resolved_key_url, None).await?;
            key_cache.insert(resolved_key_url.clone(), fetched.clone());
            fetched
        };
        if key_bytes.len() != 16 {
            return Err(DownloadError::SegmentFailed(format!(
                "expected 16-byte HLS AES-128 key, got {} bytes",
                key_bytes.len()
            )));
        }

        let iv = if let Some(iv) = &key.iv {
            parse_hls_iv(iv)?
        } else {
            media_sequence_to_iv(media_sequence)
        };

        Ok((Some(key_bytes), Some(iv)))
    }

    async fn build_dash_plan(&self, manifest_url: &str) -> Result<StreamingPlan, DownloadError> {
        let text = self.fetch_text(manifest_url).await?;
        let mpd: MPD =
            parse_mpd(&text).map_err(|err| DownloadError::SegmentFailed(format!("failed to parse DASH MPD: {err}")))?;

        if mpd.mpdtype.as_deref() == Some("dynamic") {
            return Err(DownloadError::SegmentFailed(
                "live DASH manifests are not supported".to_string(),
            ));
        }

        let period = mpd
            .periods
            .first()
            .ok_or_else(|| DownloadError::SegmentFailed("DASH MPD has no periods".to_string()))?;

        let video_sets: Vec<&AdaptationSet> = period
            .adaptations
            .iter()
            .filter(|set| adaptation_kind(set).as_deref() == Some("video"))
            .collect();
        let audio_sets: Vec<&AdaptationSet> = period
            .adaptations
            .iter()
            .filter(|set| adaptation_kind(set).as_deref() == Some("audio"))
            .collect();

        if !video_sets.is_empty() && !audio_sets.is_empty() {
            return Err(DownloadError::SegmentFailed(
                "DASH manifests with separate audio and video adaptation sets are not supported yet".to_string(),
            ));
        }

        let adaptation = video_sets
            .first()
            .copied()
            .or_else(|| audio_sets.first().copied())
            .or_else(|| period.adaptations.first())
            .ok_or_else(|| DownloadError::SegmentFailed("DASH MPD has no adaptation sets".to_string()))?;

        let representation = adaptation
            .representations
            .iter()
            .max_by_key(|representation| representation.bandwidth.unwrap_or(0))
            .ok_or_else(|| DownloadError::SegmentFailed("DASH adaptation set has no representations".to_string()))?;

        let template = representation
            .SegmentTemplate
            .as_ref()
            .or(adaptation.SegmentTemplate.as_ref())
            .or(period.SegmentTemplate.as_ref())
            .ok_or_else(|| {
                DownloadError::SegmentFailed(
                    "only DASH SegmentTemplate manifests are supported right now".to_string(),
                )
            })?;

        let base_url = dash_base_url(&mpd, period, adaptation, representation, manifest_url)?;
        let timescale = template.timescale.unwrap_or(1);
        let start_number = template.startNumber.unwrap_or(1);

        let mut jobs = Vec::new();
        let mut ordinal = 0_i64;

        if let Some(initialization) = &template.initialization {
            jobs.push(StreamingSegmentJob {
                id: "init00000".to_string(),
                ordinal,
                url: resolve_url(&base_url, &substitute_dash_template(
                    initialization,
                    representation.id.as_deref(),
                    representation.bandwidth,
                    start_number,
                    0,
                ))?,
                byte_range: None,
                decryption_key: None,
                decryption_iv: None,
                stream_type: StreamType::Primary,
            });
            ordinal += 1;
        }

        let media_template = template.media.as_deref().ok_or_else(|| {
            DownloadError::SegmentFailed("DASH SegmentTemplate is missing media template".to_string())
        })?;

        if let Some(timeline) = &template.SegmentTimeline {
            let mut number = start_number;
            let mut current_time = 0_u64;
            for segment in &timeline.segments {
                if let Some(start_time) = segment.t {
                    current_time = start_time;
                }
                let repeats = segment.r.unwrap_or(0).max(0) as u64;
                for _ in 0..=repeats {
                    jobs.push(StreamingSegmentJob {
                        id: format!("seg{:05}", jobs.len()),
                        ordinal,
                        url: resolve_url(&base_url, &substitute_dash_template(
                            media_template,
                            representation.id.as_deref(),
                            representation.bandwidth,
                            number,
                            current_time,
                        ))?,
                        byte_range: None,
                        decryption_key: None,
                        decryption_iv: None,
                        stream_type: StreamType::Primary,
                    });
                    ordinal += 1;
                    number += 1;
                    current_time = current_time.saturating_add(segment.d);
                }
            }
        } else if let Some(duration_units) = template.duration {
            let total_seconds = period
                .duration
                .or(mpd.mediaPresentationDuration)
                .map(|duration| duration.as_secs_f64())
                .ok_or_else(|| {
                    DownloadError::SegmentFailed(
                        "DASH manifest needs either SegmentTimeline or mediaPresentationDuration".to_string(),
                    )
                })?;
            let segment_seconds = duration_units / timescale as f64;
            let segment_count = (total_seconds / segment_seconds).ceil() as u64;

            for index in 0..segment_count {
                let number = start_number + index;
                let time = index * duration_units as u64;
                jobs.push(StreamingSegmentJob {
                    id: format!("seg{:05}", jobs.len()),
                    ordinal,
                    url: resolve_url(&base_url, &substitute_dash_template(
                        media_template,
                        representation.id.as_deref(),
                        representation.bandwidth,
                        number,
                        time,
                    ))?,
                    byte_range: None,
                    decryption_key: None,
                    decryption_iv: None,
                    stream_type: StreamType::Primary,
                });
                ordinal += 1;
            }
        } else {
            return Err(DownloadError::SegmentFailed(
                "only DASH SegmentTemplate manifests with SegmentTimeline or duration are supported".to_string(),
            ));
        }

        if jobs.is_empty() {
            return Err(DownloadError::SegmentFailed(
                "DASH manifest produced no segment jobs".to_string(),
            ));
        }

        let mime = representation
            .mimeType
            .clone()
            .or_else(|| adaptation.mimeType.clone());
        let output_extension =
            infer_stream_extension(DownloadKind::Dash, jobs.iter().map(|job| job.url.as_str()), mime.as_deref());

        Ok(StreamingPlan {
            jobs,
            output_extension,
            content_type: mime,
        })
    }

    async fn fetch_text(&self, url: &str) -> Result<String, DownloadError> {
        let response = self.client.get(url).send().await?;
        let response = response.error_for_status()?;
        response.text().await.map_err(DownloadError::from)
    }

    async fn fetch_bytes(
        &self,
        url: &str,
        byte_range: Option<(u64, u64)>,
    ) -> Result<Vec<u8>, DownloadError> {
        const MAX_RETRIES: usize = 3;
        let mut retries = 0;

        loop {
            if self.cancel_token.is_cancelled() {
                return Err(DownloadError::Cancelled);
            }

            let mut builder = self.client.get(url);
            if let Some((start, end)) = byte_range {
                builder = builder.header("Range", format!("bytes={start}-{end}"));
            }

            match builder.send().await {
                Ok(response) => {
                    let status = response.status();
                    if !status.is_success() {
                        retries += 1;
                        if retries >= MAX_RETRIES {
                            return Err(DownloadError::MaxRetryExceeded);
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(retry_backoff_ms(retries))).await;
                        continue;
                    }

                    if byte_range.is_some() && status != StatusCode::PARTIAL_CONTENT {
                        return Err(DownloadError::SegmentFailed(format!(
                            "server ignored HLS/DASH byte range request for {url}"
                        )));
                    }

                    return response
                        .bytes()
                        .await
                        .map(|bytes| bytes.to_vec())
                        .map_err(DownloadError::from);
                }
                Err(err) => {
                    retries += 1;
                    if retries >= MAX_RETRIES {
                        return Err(DownloadError::Network(err));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(retry_backoff_ms(retries))).await;
                }
            }
        }
    }

    async fn write_job(
        &self,
        job: StreamingSegmentJob,
        progress_tx: Option<mpsc::Sender<Result<ProgressEvent, String>>>,
    ) -> Result<Segment, DownloadError> {
        let mut bytes = self.fetch_bytes(&job.url, job.byte_range).await?;
        if let (Some(key), Some(iv)) = (job.decryption_key.as_deref(), job.decryption_iv) {
            bytes = decrypt_hls_segment(bytes, key, &iv)?;
        }
        if self.cancel_token.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }

        let path = {
            let state = self.state.read().unwrap();
            PathBuf::from(&state.temp_dir).join(&job.id)
        };
        tokio::fs::write(&path, &bytes).await.map_err(DownloadError::Disk)?;
        let len = bytes.len() as u64;

        if let Some(tx) = progress_tx {
            let _ = tx.try_send(Ok(ProgressEvent {
                segment_id: job.id.clone(),
                offset: job.ordinal.max(0) as u64,
                bytes_delta: len,
                total_bytes: Some(len),
            }));
        }

        Ok(Segment {
            id: job.id,
            offset: job.ordinal,
            length: len as i64,
            downloaded: len as i64,
            state: SegmentState::Finished,
            stream_type: job.stream_type,
        })
    }

    async fn recover_existing_segments(&self, jobs: &[StreamingSegmentJob], temp_dir_path: &str) {
        let temp_dir = PathBuf::from(temp_dir_path);
        let mut current = self.segments.write().await;
        let persisted = current.clone();
        current.clear();

        for job in jobs {
            let existing = persisted.get(&job.id).cloned();
            let path = temp_dir.join(&job.id);
            let restored = std::fs::metadata(&path).ok().map(|meta| meta.len() as i64).unwrap_or(0);
            let mut segment = existing.unwrap_or_else(|| Segment {
                id: job.id.clone(),
                offset: job.ordinal,
                length: -1,
                downloaded: 0,
                state: SegmentState::NotStarted,
                stream_type: job.stream_type,
            });
            segment.offset = job.ordinal;
            segment.stream_type = job.stream_type;
            if restored > 0 {
                segment.length = restored;
                segment.downloaded = restored;
                segment.state = SegmentState::Finished;
            } else {
                segment.downloaded = 0;
                segment.state = SegmentState::NotStarted;
            }
            current.insert(segment.id.clone(), segment);
        }
    }
}

#[async_trait]
impl DownloadStrategy for StreamingDownloadStrategy {
    fn set_progress_tx(&self, tx: mpsc::Sender<Result<ProgressEvent, String>>) {
        *self.progress_tx.lock().unwrap() = Some(tx);
    }

    fn clear_progress_tx(&self) {
        *self.progress_tx.lock().unwrap() = None;
    }

    async fn preprocess(&self) -> Result<(), DownloadError> {
        probe_if_needed(&self.state, &self.client).await?;
        self.state.write().unwrap().set_phase(DownloadPhase::Segmenting);

        let temp_dir_path = self.state.read().unwrap().temp_dir.clone();
        tokio::fs::create_dir_all(&temp_dir_path)
            .await
            .map_err(DownloadError::Disk)?;

        let plan = self.rebuild_plan().await?;
        {
            let mut state = self.state.write().unwrap();
            state.resumable = true;
            if state.download_kind == DownloadKind::Direct {
                state.download_kind = detect_download_kind(&state.url, state.content_type.as_deref());
            }
            state.content_type = plan.content_type.clone().or(state.content_type.clone());
        }
        self.recover_existing_segments(&plan.jobs, &temp_dir_path).await;
        *self.plan.write().await = Some(plan);
        Ok(())
    }

    async fn download(&self) -> Result<(), DownloadError> {
        self.state.write().unwrap().set_phase(DownloadPhase::Downloading { progress: None });

        let plan = self
            .plan
            .read()
            .await
            .clone()
            .ok_or(DownloadError::InvalidState)?;
        let progress_tx = self.progress_tx.lock().unwrap().clone();

        let current = self.segments.read().await.clone();
        let pending_jobs: Vec<_> = plan
            .jobs
            .iter()
            .filter(|job| current.get(&job.id).map(|segment| segment.state != SegmentState::Finished).unwrap_or(true))
            .cloned()
            .collect();
        if pending_jobs.is_empty() {
            return Ok(());
        }

        let semaphore = Arc::new(Semaphore::new(self.connections.max(1)));
        let mut handles = Vec::with_capacity(pending_jobs.len());
        for job in pending_jobs {
            let permit = semaphore.clone().acquire_owned().await.unwrap();
            let this = self.clone();
            let tx = progress_tx.clone();
            handles.push(tokio::spawn(async move {
                let _permit = permit;
                let id = job.id.clone();
                let result = this.write_job(job, tx).await;
                (id, result)
            }));
        }

        let mut first_error: Option<DownloadError> = None;
        let mut segments = self.segments.write().await;
        for handle in handles {
            match handle.await {
                Ok((id, Ok(segment))) => {
                    segments.insert(id, segment);
                }
                Ok((id, Err(err))) => {
                    if let Some(segment) = segments.get_mut(&id) {
                        segment.state = SegmentState::Failed;
                    }
                    if first_error.is_none() {
                        first_error = Some(err);
                    }
                }
                Err(err) => {
                    if first_error.is_none() {
                        first_error = Some(DownloadError::SegmentFailed(err.to_string()));
                    }
                }
            }
        }
        drop(segments);

        if let Some(err) = first_error {
            if let Some(tx) = &progress_tx {
                let _ = tx.try_send(Err(err.to_string()));
            }
            return Err(err);
        }
        Ok(())
    }

    async fn pause(&self) -> Result<(), DownloadError> {
        self.cancel_token.cancel();
        Ok(())
    }

    async fn stop(&self) -> Result<(), DownloadError> {
        self.cancel_token.cancel();
        Ok(())
    }

    async fn postprocess(&self) -> Result<(), DownloadError> {
        self.state.write().unwrap().set_phase(DownloadPhase::Assembling);
        let plan = self
            .plan
            .read()
            .await
            .clone()
            .ok_or(DownloadError::InvalidState)?;

        let (segment_ids, temp_dir, output_file) = {
            let segments = self.segments.read().await;
            for segment in segments.values() {
                if segment.state != SegmentState::Finished {
                    return Err(DownloadError::SegmentFailed(format!(
                        "segment {} is in state {:?}, expected Finished",
                        segment.id, segment.state
                    )));
                }
            }

            let mut sorted: Vec<_> = segments.values().collect();
            sorted.sort_by_key(|segment| segment.offset);
            let segment_ids = sorted.iter().map(|segment| segment.id.clone()).collect::<Vec<_>>();

            let state = self.state.read().unwrap();
            let output_file = resolve_stream_output_path(&state.output_path, &plan.output_extension);
            (segment_ids, state.temp_dir.clone(), output_file)
        };
        self.state.write().unwrap().output_path = Some(output_file.clone());

        tokio::task::spawn_blocking(move || {
            use std::fs::File;
            use std::io::Write;

            let mut output = File::create(&output_file)?;
            for segment_id in &segment_ids {
                let path = PathBuf::from(&temp_dir).join(segment_id);
                let mut input = File::open(&path)?;
                std::io::copy(&mut input, &mut output)?;
            }
            output.flush()?;
            for segment_id in &segment_ids {
                let path = PathBuf::from(&temp_dir).join(segment_id);
                let _ = std::fs::remove_file(path);
            }
            let _ = std::fs::remove_dir(&temp_dir);
            Ok::<(), std::io::Error>(())
        })
        .await
        .map_err(|err| DownloadError::SegmentFailed(err.to_string()))?
        .map_err(DownloadError::Disk)?;

        self.clear_progress_tx();
        Ok(())
    }

    fn current_state(&self) -> DownloaderState {
        self.state.read().unwrap().clone()
    }

    async fn current_segments(&self) -> Vec<Segment> {
        self.segments.read().await.values().cloned().collect()
    }
}

impl Clone for StreamingDownloadStrategy {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
            segments: Arc::clone(&self.segments),
            client: Arc::clone(&self.client),
            cancel_token: self.cancel_token.clone(),
            progress_tx: StdMutex::new(self.progress_tx.lock().unwrap().clone()),
            connections: self.connections,
            plan: Arc::clone(&self.plan),
        }
    }
}

fn resolve_stream_output_path(output_path: &Option<String>, output_extension: &str) -> String {
    let base = output_path
        .clone()
        .unwrap_or_else(|| format!("download.{}", output_extension));
    let path = PathBuf::from(&base);
    match path.extension().and_then(|ext| ext.to_str()).map(|ext| ext.to_ascii_lowercase()) {
        Some(ext) if ext != "m3u8" && ext != "mpd" => base,
        Some(_) => replace_extension(&base, output_extension),
        None => format!("{}.{}", base, output_extension),
    }
}

fn retry_backoff_ms(attempt: usize) -> u64 {
    100u64 * (1u64 << attempt.min(5))
}

fn resolve_url(base: &str, reference: &str) -> Result<String, DownloadError> {
    if let Ok(url) = Url::parse(reference) {
        return Ok(url.to_string());
    }
    Url::parse(base)
        .and_then(|url| url.join(reference))
        .map(|url| url.to_string())
        .map_err(|err| DownloadError::SegmentFailed(format!("failed to resolve URL {reference}: {err}")))
}

fn infer_stream_extension<'a>(
    kind: DownloadKind,
    urls: impl Iterator<Item = &'a str>,
    content_type: Option<&str>,
) -> String {
    for url in urls {
        if let Some(path) = url.split('?').next() {
            if let Some(ext) = PathBuf::from(path)
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.to_ascii_lowercase())
            {
                let normalized = match ext.as_str() {
                    "m4s" | "cmfv" => "mp4",
                    "cmfa" => "m4a",
                    "m3u8" | "mpd" => continue,
                    other => other,
                };
                return normalized.to_string();
            }
        }
    }

    if let Some(ext) = ext_from_mime(content_type) {
        if ext != "m3u8" && ext != "mpd" {
            return ext;
        }
    }

    match kind {
        DownloadKind::Hls => "ts".to_string(),
        DownloadKind::Dash => "mp4".to_string(),
        DownloadKind::Direct => "bin".to_string(),
    }
}

fn parse_hls_iv(value: &str) -> Result<[u8; 16], DownloadError> {
    let trimmed = value.trim().strip_prefix("0x").or_else(|| value.trim().strip_prefix("0X")).unwrap_or(value.trim());
    if trimmed.len() > 32 {
        return Err(DownloadError::SegmentFailed("HLS IV is longer than 16 bytes".to_string()));
    }
    let padded = format!("{trimmed:0>32}");
    let mut iv = [0u8; 16];
    for (idx, chunk) in padded.as_bytes().chunks(2).enumerate() {
        let hex = std::str::from_utf8(chunk)
            .map_err(|err| DownloadError::SegmentFailed(format!("invalid HLS IV encoding: {err}")))?;
        iv[idx] = u8::from_str_radix(hex, 16)
            .map_err(|err| DownloadError::SegmentFailed(format!("invalid HLS IV value: {err}")))?;
    }
    Ok(iv)
}

fn media_sequence_to_iv(sequence: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&sequence.to_be_bytes());
    iv
}

fn decrypt_hls_segment(mut bytes: Vec<u8>, key: &[u8], iv: &[u8; 16]) -> Result<Vec<u8>, DownloadError> {
    let decrypted = Aes128CbcDec::new_from_slices(key, iv)
        .map_err(|err| DownloadError::SegmentFailed(format!("failed to initialize AES-128 decryptor: {err}")))?
        .decrypt_padded_mut::<Pkcs7>(&mut bytes)
        .map(|slice| slice.to_vec())
        .or_else(|_| {
            Aes128CbcDec::new_from_slices(key, iv)
                .map_err(|err| DownloadError::SegmentFailed(format!("failed to initialize AES-128 decryptor: {err}")))?
                .decrypt_padded_mut::<NoPadding>(&mut bytes)
                .map(|slice| slice.to_vec())
                .map_err(|err| DownloadError::SegmentFailed(format!("failed to decrypt HLS segment: {err}")))
        })?;
    Ok(decrypted)
}

fn resolve_hls_byte_range(
    url: &str,
    length: u64,
    offset: Option<u64>,
    state: &mut Option<(String, u64)>,
) -> Result<(u64, u64), DownloadError> {
    let start = if let Some(offset) = offset {
        offset
    } else if let Some((previous_url, previous_end)) = state.as_ref() {
        if previous_url == url {
            previous_end.saturating_add(1)
        } else {
            return Err(DownloadError::SegmentFailed(
                "HLS byte range without explicit offset cannot switch resources".to_string(),
            ));
        }
    } else {
        return Err(DownloadError::SegmentFailed(
            "HLS byte range is missing the initial offset".to_string(),
        ));
    };
    let end = start.saturating_add(length.saturating_sub(1));
    *state = Some((url.to_string(), end));
    Ok((start, end))
}

fn adaptation_kind(adaptation: &AdaptationSet) -> Option<String> {
    adaptation
        .contentType
        .clone()
        .or_else(|| {
            adaptation.mimeType.as_ref().map(|mime| {
                if mime.starts_with("video/") {
                    "video".to_string()
                } else if mime.starts_with("audio/") {
                    "audio".to_string()
                } else {
                    mime.clone()
                }
            })
        })
        .map(|value| value.to_ascii_lowercase())
}

fn dash_base_url(
    mpd: &MPD,
    period: &Period,
    adaptation: &AdaptationSet,
    representation: &Representation,
    manifest_url: &str,
) -> Result<String, DownloadError> {
    let mut current = manifest_url.to_string();
    for base in mpd
        .base_url
        .iter()
        .map(|base| base.base.as_str())
        .chain(period.BaseURL.iter().map(|base| base.base.as_str()))
        .chain(adaptation.BaseURL.iter().map(|base| base.base.as_str()))
        .chain(representation.BaseURL.iter().map(|base| base.base.as_str()))
    {
        current = resolve_url(&current, base)?;
    }
    Ok(current)
}

fn substitute_dash_template(
    template: &str,
    representation_id: Option<&str>,
    bandwidth: Option<u64>,
    number: u64,
    time: u64,
) -> String {
    let mut output = template.to_string();
    if let Some(id) = representation_id {
        output = output.replace("$RepresentationID$", id);
    }
    if let Some(bandwidth) = bandwidth {
        output = output.replace("$Bandwidth$", &bandwidth.to_string());
    }
    output = replace_dash_token(output, "Number", number);
    replace_dash_token(output, "Time", time)
}

fn replace_dash_token(template: String, token: &str, value: u64) -> String {
    let plain = format!("${token}$");
    if template.contains(&plain) {
        return template.replace(&plain, &value.to_string());
    }

    let prefix = format!("${token}%0");
    if let Some(start) = template.find(&prefix) {
        if let Some(end_rel) = template[start..].find("$") {
            let end = start + end_rel;
            let spec = &template[start + prefix.len()..end];
            if let Some(width) = spec.strip_suffix('d').and_then(|digits| digits.parse::<usize>().ok()) {
                let formatted = format!("{value:0width$}");
                return format!("{}{}{}", &template[..start], formatted, &template[end + 1..]);
            }
        }
    }
    template
}
