# prtui-gitlab

GitLab provider for prtui. Authentication uses the token configured by `glab`
for the repository host.

## Live tests

The ignored integration tests use the real provider and an authenticated GitLab
instance. Set `PRTUI_GITLAB_TEST_REPO` to `[HOST/]NAMESPACE/PROJECT` and
`PRTUI_GITLAB_TEST_MR` to an open merge request number.

```sh
export PRTUI_GITLAB_TEST_REPO=gitlab.tale.me/root/prtui-test
export PRTUI_GITLAB_TEST_MR=2
cargo test -p prtui-gitlab --test live read_live_merge_request -- --ignored --nocapture
```

`exercise_live_review` creates and publishes at least 105 comments, edits drafts
and published notes, replies, resolves and reopens threads, and deletes test notes.
It requires an MR without pending drafts. Published test comments remain for
inspection. Use a disposable MR; the test does not create or merge branches.

```sh
PRTUI_GITLAB_TEST_WRITE=yes PRTUI_GITLAB_TEST_SEED=12648430 \
  cargo test -p prtui-gitlab --test live exercise_live_review -- --ignored --nocapture
```

The seed controls multiline selections; single-line cases cover the returned
patches. A fixture with more than 100 changed files also exercises file pagination.
Failures print the case number, path, and anchor.

To compare decoded multiline positions with raw GitLab responses, set the numeric
project ID and run after the write test finishes:

```sh
PRTUI_GITLAB_TEST_PROJECT_ID=1 \
  cargo test -p prtui-gitlab --test live compare_live_positions -- --ignored --nocapture
```

`exercise_live_review_states` checks requested changes, approval, and a subsequent
comment-only review, both with and without drafts. Assign the authenticated user
as a reviewer on the fixture MR first.

```sh
PRTUI_GITLAB_TEST_WRITE=yes \
  cargo test -p prtui-gitlab --test live exercise_live_review_states -- --ignored --nocapture
```

`reports_unrecorded_live_review` additionally requires the numeric project ID and
a self-authored fixture MR. It temporarily removes the current user from reviewers,
checks that a successful API response without a stored verdict reports a partial
failure, then restores the reviewers and submits a comment-only review.

Review submission sends `requested_changes` or `reviewed` to GitLab. Requesting
changes also checks the current user's persisted reviewer state. A failure to
record or read back the verdict reports that comments were already published.
Merge-blocking enforcement remains the GitLab server's responsibility.
