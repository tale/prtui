use prtui_core::{Provider, PullRequestListItem, Repo, ReviewStatus};
use prtui_tui::selector::Message;
use std::collections::{HashMap, HashSet};
use tokio::task::JoinSet;

const CONCURRENCY: usize = 4;
type Key = (String, u32);

#[derive(Default)]
pub struct Loader {
    generation: u64,
    requested: HashSet<Key>,
    cached: HashMap<Key, PullRequestListItem>,
    tasks: JoinSet<Message>,
}

impl Loader {
    /// Starts a fresh listing while retaining details until replacements arrive.
    pub fn refresh<P: Provider>(
        &mut self,
        provider: P,
        repo: Option<Repo>,
        generation: u64,
    ) {
        self.tasks.abort_all();
        self.generation = generation;
        self.requested.clear();
        self.tasks.spawn(async move {
            let listed = match repo {
                Some(repo) => {
                    provider.repository_pull_requests(repo, false).await
                }
                None => provider.user_pull_requests(false).await,
            }
            .map_err(|error| error.to_string());
            Message::Listed(generation, listed)
        });
    }

    /// Schedules visible rows that have not been requested in this generation.
    pub fn visible<P: Provider>(
        &mut self,
        provider: P,
        generation: u64,
        rows: &[PullRequestListItem],
    ) {
        if generation != self.generation {
            return;
        }
        while self.tasks.len() < CONCURRENCY {
            let items = self.batch(rows);
            if items.is_empty() {
                return;
            }
            self.tasks.spawn(async move {
                Message::Details(
                    generation,
                    provider
                        .fetch_listing_details(items)
                        .await
                        .map_err(|error| error.to_string()),
                )
            });
        }
    }

    fn batch(
        &mut self,
        rows: &[PullRequestListItem],
    ) -> Vec<PullRequestListItem> {
        let mut items: Vec<PullRequestListItem> = Vec::new();
        for row in rows {
            let key = (row.target.repo.slug(), row.target.number);
            if self.requested.contains(&key) {
                continue;
            }
            if items.first().is_some_and(|first| {
                first.target.repo.host != row.target.repo.host
            }) {
                continue;
            }
            self.requested.insert(key);
            items.push(row.clone());
        }
        items
    }

    /// Yields coherent updates, ignoring work superseded by a refresh.
    pub async fn next(&mut self) -> Message {
        loop {
            let Some(result) = self.tasks.join_next().await else {
                return std::future::pending().await;
            };
            let mut message = match result {
                Ok(message) => message,
                Err(error) if error.is_cancelled() => continue,
                Err(error) => return Message::Failed(error.to_string()),
            };
            if self.accept(&mut message) {
                return message;
            }
        }
    }

    fn accept(&mut self, message: &mut Message) -> bool {
        match message {
            Message::Listed(generation, outcome) => {
                if *generation != self.generation {
                    return false;
                }
                let Ok(list) = outcome else {
                    return true;
                };
                for row in &mut list.items {
                    if row.changes.is_some() {
                        continue;
                    }
                    let key = (row.target.repo.slug(), row.target.number);
                    let Some(old) = self.cached.get(&key) else {
                        continue;
                    };
                    row.changes = old.changes;
                    if matches!(row.review_status, ReviewStatus::Loading)
                        && !matches!(old.review_status, ReviewStatus::Draft)
                    {
                        row.review_status = old.review_status.clone();
                    }
                }
                self.cached = list
                    .items
                    .iter()
                    .map(|row| {
                        (
                            (row.target.repo.slug(), row.target.number),
                            row.clone(),
                        )
                    })
                    .collect();
            }
            Message::Details(generation, outcome) => {
                if *generation != self.generation {
                    return false;
                }
                if let Ok(rows) = outcome {
                    for row in rows {
                        let key = (row.target.repo.slug(), row.target.number);
                        if let Some(old) = self.cached.get_mut(&key) {
                            old.review_status = row.review_status.clone();
                            old.changes = row.changes;
                        }
                    }
                }
            }
            _ => {}
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prtui_core::{
        Changes, PullRequestList, PullRequestListScope, PullRequestTarget,
    };
    use std::sync::Arc;

    fn row(number: u32, host: Option<&str>) -> PullRequestListItem {
        PullRequestListItem {
            id: format!("PR_{number}").into(),
            target: PullRequestTarget {
                repo: Arc::new(Repo {
                    host: host.map(str::to_owned),
                    namespace: "owner".into(),
                    name: "repo".into(),
                }),
                number,
            },
            title: format!("Change {number}"),
            author: "author".into(),
            review_status: ReviewStatus::Loading,
            changes: None,
            review_request: None,
        }
    }

    fn listing(generation: u64, items: Vec<PullRequestListItem>) -> Message {
        Message::Listed(
            generation,
            Ok(PullRequestList {
                scope: PullRequestListScope::Repository,
                items,
            }),
        )
    }

    #[test]
    fn visible_rows_are_one_batch_and_remain_cached_across_scrolling() {
        let mut loader = Loader::default();
        let rows: Vec<_> = (1..=40).map(|number| row(number, None)).collect();
        assert_eq!(loader.batch(&rows[..25]).len(), 25);
        assert!(loader.batch(&rows[..25]).is_empty());
        let next = loader.batch(&rows[20..30]);
        assert_eq!(
            next.iter().map(|row| row.target.number).collect::<Vec<_>>(),
            [26, 27, 28, 29, 30]
        );
        assert!(loader.batch(&rows[..25]).is_empty());
        assert_eq!(loader.requested.len(), 30);
    }

    #[test]
    fn request_batches_never_mix_credentials_between_hosts() {
        let mut loader = Loader::default();
        let rows = [
            row(1, None),
            row(1, Some("github.example.com")),
            row(2, None),
        ];
        let first = loader.batch(&rows);
        assert_eq!(first.len(), 2);
        assert!(first.iter().all(|row| row.target.repo.host.is_none()));
        let second = loader.batch(&rows);
        assert_eq!(second.len(), 1);
        assert_eq!(
            second[0].target.repo.host.as_deref(),
            Some("github.example.com")
        );
        assert!(loader.batch(&rows).is_empty());
    }

    #[test]
    fn refreshing_retains_details_and_removes_rows_no_longer_listed() {
        let mut loader = Loader::default();
        loader.accept(&mut listing(0, vec![row(1, None), row(2, None)]));
        let mut detailed = row(1, None);
        detailed.review_status = ReviewStatus::Approved;
        detailed.changes = Some(Changes {
            additions: 7,
            deletions: 3,
        });
        loader.accept(&mut Message::Details(0, Ok(vec![detailed])));
        loader.generation = 1;
        let mut fresh = listing(1, vec![row(1, None)]);
        assert!(loader.accept(&mut fresh));
        let Message::Listed(_, Ok(list)) = fresh else {
            panic!("expected listing");
        };
        assert!(matches!(
            list.items[0].review_status,
            ReviewStatus::Approved
        ));
        assert_eq!(list.items[0].changes.unwrap().additions, 7);
        assert_eq!(loader.cached.len(), 1);
        let mut draft = row(1, None);
        draft.review_status = ReviewStatus::Draft;
        let mut fresh = listing(1, vec![draft]);
        loader.accept(&mut fresh);
        let Message::Listed(_, Ok(list)) = fresh else {
            panic!("expected listing");
        };
        assert!(matches!(list.items[0].review_status, ReviewStatus::Draft));
    }

    #[tokio::test]
    async fn stale_results_are_ignored_and_batches_arrive_as_one_update() {
        let mut loader = Loader {
            generation: 1,
            ..Loader::default()
        };
        loader.accept(&mut listing(1, vec![row(1, None), row(2, None)]));
        assert!(!loader.accept(&mut listing(0, vec![])));
        assert!(!loader.accept(&mut Message::Details(0, Err("stale".into()))));
        loader
            .tasks
            .spawn(async { Message::Details(0, Ok(vec![row(1, None)])) });
        loader.tasks.spawn(async {
            Message::Details(1, Ok(vec![row(1, None), row(2, None)]))
        });
        let Message::Details(generation, Ok(rows)) = loader.next().await else {
            panic!("expected current batch");
        };
        assert_eq!(generation, 1);
        assert_eq!(rows.len(), 2);
    }

    #[tokio::test]
    async fn capacity_does_not_mark_unscheduled_rows_as_requested() {
        let mut loader = Loader::default();
        for _ in 0..CONCURRENCY {
            loader.tasks.spawn(std::future::pending());
        }
        loader.visible(prtui_github::GitHub, 0, &[row(1, None)]);
        assert!(loader.requested.is_empty());
    }
}
