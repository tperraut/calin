//! Telling whether two git URLs name the same repository.

/// A key that is equal for every spelling of a repository's URL: the ssh
/// shorthand, `ssh://`, `https://`, with or without `.git` or a trailing
/// slash. The host compares without case, the path exactly; the scheme, the
/// user and the port say how to reach the repository, not which one it is.
///
/// A local path and its `file://` URL share a key too. Anything that cannot
/// be taken apart is its own key, so it still matches itself.
pub(crate) fn key(url: &str) -> String {
    let url = url.trim();
    if let Some((scheme, rest)) = url.split_once("://") {
        if scheme.eq_ignore_ascii_case("file") {
            // `file://host/path` exists, but only ever with an empty host.
            let path = &rest[rest.find('/').unwrap_or(rest.len())..];
            return local(path);
        }
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        return remote(host_of(authority), path);
    }
    if url.starts_with(['/', '.', '~']) {
        return local(url);
    }
    // `[user@]host:path`, git's shorthand for ssh. A slash before the colon
    // means the colon is part of a path instead.
    match url.split_once(':') {
        Some((authority, path)) if !authority.is_empty() && !authority.contains('/') => {
            remote(host_of(authority), path)
        }
        _ => local(url),
    }
}

/// The host of `[user[:password]@]host[:port]`.
fn host_of(authority: &str) -> &str {
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if host.starts_with('[') {
        // An IPv6 literal keeps its brackets, and its colons.
        return &host[..host.find(']').map_or(host.len(), |end| end + 1)];
    }
    host.split_once(':').map_or(host, |(host, _port)| host)
}

fn remote(host: &str, path: &str) -> String {
    format!(
        "{}/{}",
        host.to_ascii_lowercase(),
        trimmed(path).trim_start_matches('/')
    )
}

fn local(path: &str) -> String {
    format!("local:{}", trimmed(path))
}

/// The path without what git itself treats as optional at its end.
fn trimmed(path: &str) -> &str {
    let path = path.trim_end_matches('/');
    path.strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_of_a_remote_has_one_key() {
        let same = [
            "git@github.com:you/api.git",
            "git@github.com:you/api",
            "ssh://git@github.com/you/api",
            "ssh://git@github.com/you/api.git",
            "ssh://git@github.com:22/you/api.git",
            "https://github.com/you/api",
            "https://github.com/you/api.git",
            "https://github.com/you/api/",
            "https://github.com/you/api.git/",
            "https://GitHub.COM/you/api",
            "https://user:token@github.com/you/api.git",
            "http://github.com/you/api",
            "git://github.com/you/api.git",
            "github.com:you/api.git",
            "  https://github.com/you/api.git\n",
        ];
        for url in same {
            assert_eq!(key(url), "github.com/you/api", "{url}");
        }
    }

    #[test]
    fn different_repositories_have_different_keys() {
        let api = key("git@github.com:you/api.git");
        let others = [
            // The path is compared exactly: these are other repositories.
            "git@github.com:You/api.git",
            "git@github.com:you/API.git",
            "git@github.com:you/api2.git",
            "git@github.com:you/ap",
            "git@github.com:other/api.git",
            "git@gitlab.com:you/api.git",
            "https://github.com.example/you/api",
            "https://github.com/you/api/extra",
            "/you/api",
            "",
        ];
        for url in others {
            assert_ne!(key(url), api, "{url}");
        }
        assert_eq!(key("git@host:group/sub/name.git"), "host/group/sub/name");
        assert_eq!(key("ssh://git@[::1]:2222/you/api.git"), "[::1]/you/api");
        assert_eq!(key("https://host"), "host/");
    }

    #[test]
    fn a_local_path_and_its_file_url_share_a_key() {
        for url in [
            "/srv/git/api.git",
            "/srv/git/api",
            "/srv/git/api/",
            "file:///srv/git/api.git",
            "file:///srv/git/api",
            "file://localhost/srv/git/api/",
        ] {
            assert_eq!(key(url), "local:/srv/git/api", "{url}");
        }
        assert_ne!(key("/srv/git/api"), key("/srv/git/web"));
        // A colon after a slash is part of the path, not a host separator.
        assert_eq!(key("./odd:name"), "local:./odd:name");
        assert_eq!(key("dir/odd:name"), "local:dir/odd:name");
        // What cannot be taken apart still equals itself.
        assert_eq!(key("nonsense"), key(" nonsense "));
    }
}
