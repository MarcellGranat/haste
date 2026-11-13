//! Per-folder UID sync with `UIDVALIDITY` handling. Pure over [`MailSession`], so it is
//! testable without a network.

use std::collections::BTreeMap;

use haste_core::connector::Batch;
use serde::{Deserialize, Serialize};

use crate::config::{FolderConfig, IcloudConfig};
use crate::parse::{build_batch, parse_message, ParsedMessage};
use crate::session::{ImapFailure, MailSession, SearchQuery};

/// Messages per FETCH round trip.
pub const FETCH_CHUNK: usize = 50;
/// Most bytes of a `text/plain` part that are ever requested.
pub const MAX_PART_BYTES: u32 = 64 * 1024;
/// Most bytes of an encoded `text/html` part that are ever requested (HTML is much larger than
/// the text it reduces to, and is often base64-encoded).
pub const MAX_HTML_BYTES: u32 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderCursor {
    pub uidvalidity: u32,
    pub last_uid: u32,
}

/// The connector `Cursor`: one entry per folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IcloudCursor {
    pub folders: BTreeMap<String, FolderCursor>,
}

impl IcloudCursor {
    #[must_use]
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// An unreadable cursor is treated as "never synced": one bounded backfill, no loop.
    #[must_use]
    pub fn decode(text: &str) -> Self {
        serde_json::from_str(text).unwrap_or_default()
    }
}

/// Counts only: no subjects, no bodies, no addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderReport {
    pub name: String,
    pub new_messages: usize,
    /// `UIDVALIDITY` changed, so this folder's cursor was discarded.
    pub reset: bool,
    /// The folder could not be opened (for example it does not exist).
    pub skipped: bool,
}

impl FolderReport {
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{}:new={},reset={},skipped={}",
            self.name, self.new_messages, self.reset, self.skipped
        )
    }
}

pub struct SyncOutcome {
    pub batch: Batch,
    pub cursor: IcloudCursor,
    pub reports: Vec<FolderReport>,
}

fn sync_folder(
    session: &mut dyn MailSession,
    config: &IcloudConfig,
    folder: &FolderConfig,
    state: Option<FolderCursor>,
) -> Result<(Vec<ParsedMessage>, Option<FolderCursor>, FolderReport), ImapFailure> {
    let mut report = FolderReport {
        name: folder.name.clone(),
        new_messages: 0,
        reset: false,
        skipped: false,
    };
    let info = match session.examine(&folder.name) {
        Ok(info) => info,
        // A missing folder must not stop the others; the old cursor is kept.
        Err(ImapFailure::Protocol(_)) => {
            report.skipped = true;
            return Ok((Vec::new(), state, report));
        }
        Err(other) => return Err(other),
    };

    let delta_from = state
        .filter(|s| s.uidvalidity == info.uid_validity)
        .map(|s| s.last_uid);
    report.reset = state.is_some() && delta_from.is_none();
    let uids: Vec<u32> = if let Some(last) = delta_from {
        // `UID n:*` always returns the newest message even if it is older than `n`.
        session
            .uid_search(SearchQuery::UidAbove(last))?
            .into_iter()
            .filter(|u| *u > last)
            .collect()
    } else {
        let all = session.uid_search(SearchQuery::All)?;
        let keep = config.backfill_for(&folder.name);
        all[all.len().saturating_sub(keep)..].to_vec()
    };

    let mut parsed = Vec::new();
    for chunk in uids.chunks(FETCH_CHUNK) {
        for fetch in session.fetch_headers(chunk)? {
            let body = match &fetch.text {
                Some(part) => {
                    let limit = if part.html {
                        MAX_HTML_BYTES
                    } else {
                        MAX_PART_BYTES
                    };
                    session.fetch_text_part(fetch.uid, part, limit)?
                }
                None => None,
            };
            if let Some(m) = parse_message(&folder.name, info.uid_validity, &fetch, body.as_deref())
            {
                parsed.push(m);
            }
        }
    }
    report.new_messages = parsed.len();
    let last_uid = uids.iter().copied().max().or(delta_from).unwrap_or(0);
    Ok((
        parsed,
        Some(FolderCursor {
            uidvalidity: info.uid_validity,
            last_uid,
        }),
        report,
    ))
}

/// One cycle over every configured folder. The cursor advances only if the whole cycle succeeds.
///
/// # Errors
/// The first non-recoverable [`ImapFailure`] (authentication or transient).
pub fn sync_all(
    session: &mut dyn MailSession,
    config: &IcloudConfig,
    cursor: &IcloudCursor,
) -> Result<SyncOutcome, ImapFailure> {
    let mut next = cursor.clone();
    let mut parsed = Vec::new();
    let mut reports = Vec::new();
    for folder in config.effective_folders() {
        let (messages, folder_cursor, report) = sync_folder(
            session,
            config,
            &folder,
            cursor.folders.get(&folder.name).copied(),
        )?;
        parsed.extend(messages);
        if let Some(c) = folder_cursor {
            next.folders.insert(folder.name.clone(), c);
        }
        reports.push(report);
    }
    let mut batch = build_batch(parsed);
    batch.next_cursor = Some(haste_core::connector::Cursor(next.encode()));
    Ok(SyncOutcome {
        batch,
        cursor: next,
        reports,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{FolderInfo, HeaderFetch, TextPart};

    /// Scripted in-memory mailbox set.
    #[derive(Default)]
    struct Fake {
        folders: BTreeMap<String, (u32, Vec<u32>)>, // name -> (uidvalidity, uids)
        searched: Vec<String>,
        fetched: Vec<u32>,
    }

    impl MailSession for Fake {
        fn move_messages(
            &mut self,
            _folder: &str,
            _uid_validity: u32,
            _uids: &[u32],
            _target: &str,
        ) -> Result<(), ImapFailure> {
            Err(ImapFailure::Protocol("not used".into()))
        }
        fn examine(&mut self, folder: &str) -> Result<FolderInfo, ImapFailure> {
            self.folders
                .get(folder)
                .map(|(v, u)| FolderInfo {
                    uid_validity: *v,
                    exists: u32::try_from(u.len()).unwrap(),
                })
                .ok_or_else(|| ImapFailure::Protocol("no such folder".into()))
        }
        fn uid_search(&mut self, q: SearchQuery) -> Result<Vec<u32>, ImapFailure> {
            self.searched.push(format!("{q:?}"));
            let current = self.folders.values().next().cloned().unwrap_or_default().1;
            Ok(match q {
                SearchQuery::All => current,
                SearchQuery::UidAbove(n) => {
                    let above: Vec<u32> = current.iter().copied().filter(|u| *u > n).collect();
                    if above.is_empty() {
                        current.last().copied().into_iter().collect()
                    } else {
                        above
                    }
                }
            })
        }
        fn fetch_headers(&mut self, uids: &[u32]) -> Result<Vec<HeaderFetch>, ImapFailure> {
            self.fetched.extend_from_slice(uids);
            Ok(uids
                .iter()
                .map(|u| HeaderFetch {
                    uid: *u,
                    header: format!("From: a@x.org\r\nSubject: s{u}\r\nMessage-ID: <m{u}@x>\r\nDate: Tue, 14 Nov 2023 22:13:20 +0000\r\n\r\n").into_bytes(),
                    text: Some(TextPart {
                        html: false, section: vec![1], charset: "utf-8".into(), encoding: "7bit".into(), octets: 4 }),
                    attachments: Vec::new(),
                })
                .collect())
        }
        fn fetch_text_part(
            &mut self,
            _u: u32,
            _p: &TextPart,
            _m: u32,
        ) -> Result<Option<Vec<u8>>, ImapFailure> {
            Ok(Some(b"body".to_vec()))
        }
        fn append(&mut self, _folder: &str, _message: &[u8]) -> Result<(), ImapFailure> {
            // the sync path must never write: a test double that records nothing would hide it
            panic!("sync must never append");
        }
        fn logout(&mut self) {}
    }

    fn fake(validity: u32, uids: &[u32]) -> Fake {
        let mut f = Fake::default();
        f.folders.insert("INBOX".into(), (validity, uids.to_vec()));
        f
    }

    fn config(backfill: usize) -> IcloudConfig {
        let mut c = IcloudConfig::new("me@icloud.com");
        c.backfill_count = backfill;
        c
    }

    #[test]
    fn first_sync_is_bounded_to_the_newest_uids() {
        let mut s = fake(7, &(1..=10).collect::<Vec<_>>());
        let out = sync_all(&mut s, &config(3), &IcloudCursor::default()).unwrap();
        assert_eq!(s.fetched, vec![8, 9, 10]);
        assert_eq!(
            out.cursor.folders["INBOX"],
            FolderCursor {
                uidvalidity: 7,
                last_uid: 10
            }
        );
        assert_eq!(out.batch.messages.len(), 3);
    }

    #[test]
    fn delta_fetches_only_uids_above_last_uid_when_uidvalidity_matches() {
        let mut s = fake(7, &[1, 2, 3, 4, 5]);
        let mut cursor = IcloudCursor::default();
        cursor.folders.insert(
            "INBOX".into(),
            FolderCursor {
                uidvalidity: 7,
                last_uid: 3,
            },
        );
        let out = sync_all(&mut s, &config(200), &cursor).unwrap();
        assert_eq!(s.fetched, vec![4, 5]);
        assert_eq!(out.cursor.folders["INBOX"].last_uid, 5);
        assert!(!out.reports[0].reset);
    }

    #[test]
    fn nothing_new_fetches_nothing_despite_the_star_quirk() {
        let mut s = fake(7, &[1, 2, 3]);
        let mut cursor = IcloudCursor::default();
        cursor.folders.insert(
            "INBOX".into(),
            FolderCursor {
                uidvalidity: 7,
                last_uid: 3,
            },
        );
        let out = sync_all(&mut s, &config(200), &cursor).unwrap();
        assert!(s.fetched.is_empty());
        assert_eq!(out.cursor.folders["INBOX"].last_uid, 3);
        assert!(out.batch.messages.is_empty());
    }

    #[test]
    fn uidvalidity_change_resets_only_that_folder() {
        let mut s = Fake::default();
        s.folders.insert("INBOX".into(), (99, vec![1, 2]));
        let mut cursor = IcloudCursor::default();
        cursor.folders.insert(
            "INBOX".into(),
            FolderCursor {
                uidvalidity: 7,
                last_uid: 50,
            },
        );
        cursor.folders.insert(
            "Archive".into(),
            FolderCursor {
                uidvalidity: 1,
                last_uid: 9,
            },
        );
        let out = sync_all(&mut s, &config(200), &cursor).unwrap();
        assert!(out.reports[0].reset);
        assert_eq!(s.fetched, vec![1, 2], "backfilled from scratch");
        assert_eq!(
            out.cursor.folders["INBOX"],
            FolderCursor {
                uidvalidity: 99,
                last_uid: 2
            }
        );
        assert_eq!(
            out.cursor.folders["Archive"],
            FolderCursor {
                uidvalidity: 1,
                last_uid: 9
            },
            "other folders untouched"
        );
    }

    #[test]
    fn user_folders_and_per_folder_backfill_are_used() {
        let mut s = Fake::default();
        s.folders.insert("INBOX".into(), (1, vec![1, 2, 3, 4]));
        let mut c = config(2);
        c.folders = vec![FolderConfig {
            name: "INBOX".into(),
            backfill: Some(3),
        }];
        sync_all(&mut s, &c, &IcloudCursor::default()).unwrap();
        assert_eq!(s.fetched, vec![2, 3, 4]);
    }

    #[test]
    fn missing_folder_is_skipped_and_the_others_still_sync() {
        let mut s = fake(1, &[1]);
        let mut c = config(10);
        c.folders = vec![FolderConfig::new("Nope"), FolderConfig::new("INBOX")];
        let out = sync_all(&mut s, &c, &IcloudCursor::default()).unwrap();
        assert!(out.reports[0].skipped && !out.reports[1].skipped);
        assert_eq!(out.batch.messages.len(), 1);
        assert!(!out.cursor.folders.contains_key("Nope"));
    }

    #[test]
    fn cursor_round_trips_and_garbage_means_never_synced() {
        let mut c = IcloudCursor::default();
        c.folders.insert(
            "INBOX".into(),
            FolderCursor {
                uidvalidity: 5,
                last_uid: 6,
            },
        );
        assert_eq!(IcloudCursor::decode(&c.encode()), c);
        assert_eq!(IcloudCursor::decode("not json"), IcloudCursor::default());
    }

    #[test]
    fn reports_contain_counts_only() {
        let r = FolderReport {
            name: "INBOX".into(),
            new_messages: 3,
            reset: false,
            skipped: false,
        };
        assert_eq!(r.describe(), "INBOX:new=3,reset=false,skipped=false");
    }
}
