//! What a CI system says about the job it runs, from its environment variables (docs/design/ci.md
//! section 8.4, item 2): the only per-system code of `hydo ci run`. Each system names the commit,
//! the branches, the change (pull or merge request) and the job's link in its own variables; one
//! function per system reads them, a few lines each. Flags of `hydo ci run` always win over what
//! is read here, and on a laptop nothing is detected (the system is `local`).
//!
//! Adding a CI system means adding its function to [`SYSTEMS`] and its lines to the table in
//! `hydo guide ci`.

/// What the environment says about the CI job (every field optional).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CiEnv {
    /// `github`, `gitlab`, `circleci`, `jenkins`, `buildkite`, `azure`; `local` when none.
    pub system: String,
    pub run_id: Option<String>,
    pub run_url: Option<String>,
    /// The commit the job checked out.
    pub commit: Option<String>,
    /// The branch the job runs for (a pull request's source branch).
    pub branch: Option<String>,
    /// A pull or merge request's target branch.
    pub base_branch: Option<String>,
    /// A commit of the target branch the system names itself (GitLab's merge-request diff base).
    pub base_commit: Option<String>,
    /// The pull or merge request's number.
    pub change: Option<String>,
    /// `pull_request`, `push`, `schedule` or `manual`.
    pub trigger: Option<String>,
}

/// A lookup of one environment variable (the process environment, or a test's table).
pub type Get<'a> = &'a dyn Fn(&str) -> Option<String>;

/// One CI system: the variable that says the job runs there, and how to read the rest.
pub struct System {
    pub name: &'static str,
    pub detect: &'static str,
    pub read: fn(Get) -> CiEnv,
}

/// Every CI system hydo reads, in the order they are tried.
pub const SYSTEMS: &[System] = &[
    System {
        name: "github",
        detect: "GITHUB_ACTIONS",
        read: github,
    },
    System {
        name: "gitlab",
        detect: "GITLAB_CI",
        read: gitlab,
    },
    System {
        name: "circleci",
        detect: "CIRCLECI",
        read: circleci,
    },
    System {
        name: "buildkite",
        detect: "BUILDKITE",
        read: buildkite,
    },
    System {
        name: "azure",
        detect: "TF_BUILD",
        read: azure,
    },
    // last: Jenkins is detected by JENKINS_URL, which a job of another system may also see
    System {
        name: "jenkins",
        detect: "JENKINS_URL",
        read: jenkins,
    },
];

/// A non-empty value.
fn v(get: Get, k: &str) -> Option<String> {
    get(k)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// `refs/heads/main` -> `main`.
fn short_ref(r: String) -> String {
    r.strip_prefix("refs/heads/")
        .map(str::to_string)
        .unwrap_or(r)
}

/// The environment of this process (the variables of [`SYSTEMS`] only).
pub fn from_process() -> CiEnv {
    detect(&|k: &str| std::env::var(k).ok())
}

/// The first system whose variable is set, read; `local` when none is.
pub fn detect(get: Get) -> CiEnv {
    for s in SYSTEMS {
        if v(get, s.detect).is_some_and(|x| x != "false") {
            let mut e = (s.read)(get);
            e.system = s.name.into();
            return e;
        }
    }
    CiEnv {
        system: "local".into(),
        ..CiEnv::default()
    }
}

fn github(get: Get) -> CiEnv {
    let event = v(get, "GITHUB_EVENT_NAME");
    // a pull request's ref is refs/pull/<n>/merge
    let change = v(get, "GITHUB_REF").and_then(|r| {
        r.strip_prefix("refs/pull/")
            .and_then(|x| x.split('/').next())
            .map(str::to_string)
    });
    let run_id = v(get, "GITHUB_RUN_ID");
    let run_url = match (
        v(get, "GITHUB_SERVER_URL"),
        v(get, "GITHUB_REPOSITORY"),
        &run_id,
    ) {
        (Some(s), Some(r), Some(id)) => Some(format!("{s}/{r}/actions/runs/{id}")),
        _ => None,
    };
    CiEnv {
        run_id,
        run_url,
        commit: v(get, "GITHUB_SHA"),
        branch: v(get, "GITHUB_HEAD_REF").or_else(|| v(get, "GITHUB_REF_NAME")),
        base_branch: v(get, "GITHUB_BASE_REF"),
        change,
        trigger: event.map(|e| match e.as_str() {
            "pull_request" | "pull_request_target" => "pull_request".into(),
            "push" => "push".into(),
            "schedule" => "schedule".into(),
            _ => "manual".into(),
        }),
        ..CiEnv::default()
    }
}

fn gitlab(get: Get) -> CiEnv {
    let src = v(get, "CI_PIPELINE_SOURCE");
    CiEnv {
        run_id: v(get, "CI_JOB_ID").or_else(|| v(get, "CI_PIPELINE_ID")),
        run_url: v(get, "CI_JOB_URL"),
        commit: v(get, "CI_COMMIT_SHA"),
        branch: v(get, "CI_MERGE_REQUEST_SOURCE_BRANCH_NAME")
            .or_else(|| v(get, "CI_COMMIT_BRANCH")),
        base_branch: v(get, "CI_MERGE_REQUEST_TARGET_BRANCH_NAME"),
        base_commit: v(get, "CI_MERGE_REQUEST_DIFF_BASE_SHA"),
        change: v(get, "CI_MERGE_REQUEST_IID"),
        trigger: src.map(|s| match s.as_str() {
            "merge_request_event" | "external_pull_request_event" => "pull_request".into(),
            "push" => "push".into(),
            "schedule" => "schedule".into(),
            _ => "manual".into(),
        }),
        ..CiEnv::default()
    }
}

fn circleci(get: Get) -> CiEnv {
    // CircleCI names the pull request by its link and does not name the target branch
    let change = v(get, "CIRCLE_PR_NUMBER").or_else(|| {
        v(get, "CIRCLE_PULL_REQUEST").and_then(|u| u.rsplit('/').next().map(str::to_string))
    });
    CiEnv {
        run_id: v(get, "CIRCLE_WORKFLOW_ID").or_else(|| v(get, "CIRCLE_BUILD_NUM")),
        run_url: v(get, "CIRCLE_BUILD_URL"),
        commit: v(get, "CIRCLE_SHA1"),
        branch: v(get, "CIRCLE_BRANCH"),
        trigger: Some(if change.is_some() {
            "pull_request".into()
        } else {
            "push".into()
        }),
        change,
        ..CiEnv::default()
    }
}

fn buildkite(get: Get) -> CiEnv {
    let change = v(get, "BUILDKITE_PULL_REQUEST").filter(|p| p != "false");
    let trigger = if change.is_some() {
        "pull_request"
    } else if v(get, "BUILDKITE_SOURCE").as_deref() == Some("schedule") {
        "schedule"
    } else {
        "push"
    };
    CiEnv {
        run_id: v(get, "BUILDKITE_BUILD_ID"),
        run_url: v(get, "BUILDKITE_BUILD_URL"),
        commit: v(get, "BUILDKITE_COMMIT"),
        branch: v(get, "BUILDKITE_BRANCH"),
        base_branch: v(get, "BUILDKITE_PULL_REQUEST_BASE_BRANCH"),
        change,
        trigger: Some(trigger.into()),
        ..CiEnv::default()
    }
}

fn azure(get: Get) -> CiEnv {
    let id = v(get, "BUILD_BUILDID");
    let run_url = match (
        v(get, "SYSTEM_COLLECTIONURI"),
        v(get, "SYSTEM_TEAMPROJECT"),
        &id,
    ) {
        (Some(c), Some(p), Some(id)) => Some(format!("{c}{p}/_build/results?buildId={id}")),
        _ => None,
    };
    CiEnv {
        run_id: id,
        run_url,
        commit: v(get, "BUILD_SOURCEVERSION"),
        branch: v(get, "SYSTEM_PULLREQUEST_SOURCEBRANCH")
            .or_else(|| v(get, "BUILD_SOURCEBRANCH"))
            .map(short_ref),
        base_branch: v(get, "SYSTEM_PULLREQUEST_TARGETBRANCH").map(short_ref),
        // a GitHub pull request has a number; an Azure Repos one only its id
        change: v(get, "SYSTEM_PULLREQUEST_PULLREQUESTNUMBER")
            .or_else(|| v(get, "SYSTEM_PULLREQUEST_PULLREQUESTID")),
        trigger: v(get, "BUILD_REASON").map(|r| match r.as_str() {
            "PullRequest" => "pull_request".into(),
            "Schedule" => "schedule".into(),
            "IndividualCI" | "BatchedCI" => "push".into(),
            _ => "manual".into(),
        }),
        ..CiEnv::default()
    }
}

fn jenkins(get: Get) -> CiEnv {
    // a multibranch pipeline names a change request by CHANGE_ID and its target by CHANGE_TARGET
    let change = v(get, "CHANGE_ID");
    CiEnv {
        run_id: match (v(get, "JOB_NAME"), v(get, "BUILD_NUMBER")) {
            (Some(j), Some(n)) => Some(format!("{j}/{n}")),
            (None, n) => n,
            (j, None) => j,
        },
        run_url: v(get, "BUILD_URL"),
        commit: v(get, "GIT_COMMIT"),
        branch: v(get, "CHANGE_BRANCH")
            .or_else(|| v(get, "BRANCH_NAME"))
            .or_else(|| v(get, "GIT_BRANCH")),
        base_branch: v(get, "CHANGE_TARGET"),
        trigger: Some(if change.is_some() {
            "pull_request".into()
        } else {
            "push".into()
        }),
        change,
        ..CiEnv::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn of(pairs: &[(&str, &str)]) -> CiEnv {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        detect(&move |k: &str| m.get(k).cloned())
    }

    #[test]
    fn nothing_set_is_local() {
        assert_eq!(of(&[]).system, "local");
        assert_eq!(of(&[("GITHUB_ACTIONS", "false")]).system, "local");
    }

    #[test]
    fn github_pull_request() {
        let e = of(&[
            ("GITHUB_ACTIONS", "true"),
            ("GITHUB_EVENT_NAME", "pull_request"),
            ("GITHUB_SHA", "9c41e07"),
            ("GITHUB_REF", "refs/pull/412/merge"),
            ("GITHUB_HEAD_REF", "fix-charge"),
            ("GITHUB_BASE_REF", "main"),
            ("GITHUB_SERVER_URL", "https://github.com"),
            ("GITHUB_REPOSITORY", "acme/shop"),
            ("GITHUB_RUN_ID", "11873625"),
        ]);
        assert_eq!(e.system, "github");
        assert_eq!(e.change.as_deref(), Some("412"));
        assert_eq!(e.branch.as_deref(), Some("fix-charge"));
        assert_eq!(e.base_branch.as_deref(), Some("main"));
        assert_eq!(e.trigger.as_deref(), Some("pull_request"));
        assert_eq!(
            e.run_url.as_deref(),
            Some("https://github.com/acme/shop/actions/runs/11873625")
        );
        let p = of(&[
            ("GITHUB_ACTIONS", "true"),
            ("GITHUB_EVENT_NAME", "schedule"),
            ("GITHUB_REF", "refs/heads/main"),
            ("GITHUB_REF_NAME", "main"),
        ]);
        assert_eq!((p.trigger.as_deref(), p.change), (Some("schedule"), None));
        assert_eq!(p.branch.as_deref(), Some("main"));
    }

    #[test]
    fn gitlab_merge_request() {
        let e = of(&[
            ("GITLAB_CI", "true"),
            ("CI_PIPELINE_SOURCE", "merge_request_event"),
            ("CI_COMMIT_SHA", "abc"),
            ("CI_MERGE_REQUEST_IID", "7"),
            ("CI_MERGE_REQUEST_SOURCE_BRANCH_NAME", "feature"),
            ("CI_MERGE_REQUEST_TARGET_BRANCH_NAME", "main"),
            ("CI_MERGE_REQUEST_DIFF_BASE_SHA", "def"),
            ("CI_JOB_ID", "99"),
            ("CI_JOB_URL", "https://gitlab.example/acme/shop/-/jobs/99"),
        ]);
        assert_eq!(e.system, "gitlab");
        assert_eq!(e.change.as_deref(), Some("7"));
        assert_eq!(e.base_commit.as_deref(), Some("def"));
        assert_eq!(e.trigger.as_deref(), Some("pull_request"));
    }

    #[test]
    fn circleci_buildkite_azure_jenkins() {
        let c = of(&[
            ("CIRCLECI", "true"),
            ("CIRCLE_SHA1", "c1"),
            ("CIRCLE_BRANCH", "feature"),
            (
                "CIRCLE_PULL_REQUEST",
                "https://github.com/acme/shop/pull/12",
            ),
        ]);
        assert_eq!(
            (c.system.as_str(), c.change.as_deref(), c.commit.as_deref()),
            ("circleci", Some("12"), Some("c1"))
        );
        let b = of(&[
            ("BUILDKITE", "true"),
            ("BUILDKITE_COMMIT", "b1"),
            ("BUILDKITE_PULL_REQUEST", "false"),
            ("BUILDKITE_SOURCE", "schedule"),
        ]);
        assert_eq!(
            (b.system.as_str(), b.change, b.trigger.as_deref()),
            ("buildkite", None, Some("schedule"))
        );
        let a = of(&[
            ("TF_BUILD", "True"),
            ("BUILD_SOURCEVERSION", "a1"),
            ("BUILD_REASON", "PullRequest"),
            ("SYSTEM_PULLREQUEST_TARGETBRANCH", "refs/heads/main"),
            ("SYSTEM_PULLREQUEST_PULLREQUESTNUMBER", "5"),
            ("BUILD_BUILDID", "321"),
            ("SYSTEM_COLLECTIONURI", "https://dev.azure.com/acme/"),
            ("SYSTEM_TEAMPROJECT", "shop"),
        ]);
        assert_eq!(a.system, "azure");
        assert_eq!(a.base_branch.as_deref(), Some("main"));
        assert_eq!(a.change.as_deref(), Some("5"));
        assert_eq!(
            a.run_url.as_deref(),
            Some("https://dev.azure.com/acme/shop/_build/results?buildId=321")
        );
        let j = of(&[
            ("JENKINS_URL", "https://ci.example/"),
            ("GIT_COMMIT", "j1"),
            ("CHANGE_ID", "44"),
            ("CHANGE_TARGET", "main"),
            ("JOB_NAME", "shop/PR-44"),
            ("BUILD_NUMBER", "3"),
        ]);
        assert_eq!(j.system, "jenkins");
        assert_eq!(j.run_id.as_deref(), Some("shop/PR-44/3"));
        assert_eq!(j.trigger.as_deref(), Some("pull_request"));
        // a Buildkite agent may also see JENKINS_URL: the more specific system wins
        let both = of(&[("BUILDKITE", "true"), ("JENKINS_URL", "x")]);
        assert_eq!(both.system, "buildkite");
    }
}
