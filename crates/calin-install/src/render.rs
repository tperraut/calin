//! The three files `install` writes. Pure: the same input gives the same bytes.

use std::fmt::Write as _;
use std::path::Path;

use crate::envfile::assignment;
use crate::input::{InstallInput, ServiceLayout};

pub const UNIT_PATH: &str = "/etc/systemd/system/calin.service";
pub const UNIT_NAME: &str = "calin.service";
pub const ENV_DIR: &str = "/etc/calin";
pub const ENV_PATH: &str = "/etc/calin/calin.env";
pub const CADDY_DIR: &str = "/etc/caddy/conf.d";
pub const CADDY_PATH: &str = "/etc/caddy/conf.d/calin.caddy";
pub const CADDYFILE_PATH: &str = "/etc/caddy/Caddyfile";
/// The line the main Caddyfile needs so that it loads [`CADDY_PATH`].
pub const CADDY_IMPORT_LINE: &str = "import /etc/caddy/conf.d/*.caddy";
/// Largest request body the proxy lets through: file saves from the editor.
pub const MAX_BODY: &str = "10MB";

/// The variables of spec section 2 that must be set in public mode.
pub const REQUIRED_VARIABLES: [&str; 4] = [
    "CALIN_PUBLIC_URL",
    "CALIN_OIDC_ISSUER",
    "CALIN_OIDC_CLIENT_ID",
    "CALIN_ALLOWED_USERS",
];
pub const SECRET_VARIABLE: &str = "CALIN_OIDC_CLIENT_SECRET";

/// systemd's default search path, with the service user's `~/.local/bin`
/// first, where per-user installers (Claude Code's among them) put CLIs.
pub(crate) fn service_path(layout: &ServiceLayout) -> String {
    format!(
        "{}/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin",
        layout.home_dir().display()
    )
}

/// `calin serve` exactly as the unit runs it.
pub(crate) fn exec_start(layout: &ServiceLayout) -> String {
    format!(
        "{} serve --bind {} --home {}",
        layout.binary().display(),
        layout.bind(),
        layout.data_dir().display()
    )
}

pub(crate) fn read_write_paths(layout: &ServiceLayout) -> Vec<&Path> {
    let mut paths = vec![layout.data_dir()];
    paths.extend(layout.extra_dirs().iter().map(|p| p.as_path()));
    paths.push(layout.home_dir());
    paths
}

/// The systemd unit. Paths are validated to need no quoting.
pub fn render_unit(layout: &ServiceLayout) -> String {
    let user = layout.user();
    let rw = read_write_paths(layout)
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let mut out = String::new();
    let lines = [
        "# Calin, written by `calin server install`. Re-run it rather than editing this file.".to_owned(),
        "# docs/specs/2026-11-27-remote-access.md, section 5.".to_owned(),
        String::new(),
        "[Unit]".to_owned(),
        "Description=Calin, a self-hosted orchestrator for coding agents".to_owned(),
        "# Sign-in fetches the provider's metadata at startup: wait for a configured network.".to_owned(),
        "Wants=network-online.target".to_owned(),
        "After=network-online.target".to_owned(),
        String::new(),
        "[Service]".to_owned(),
        "Type=simple".to_owned(),
        "# A dedicated unprivileged user: agents and their shells run as it.".to_owned(),
        format!("User={user}"),
        format!("Group={user}"),
        format!("WorkingDirectory={}", layout.home_dir().display()),
        "# Public mode settings (spec section 2). Root-only, mode 0600: it holds the client secret.".to_owned(),
        format!("EnvironmentFile={ENV_PATH}"),
        "# Agent CLIs installed per user (Claude Code's installer, for one) live in ~/.local/bin.".to_owned(),
        format!("Environment=PATH={}", service_path(layout)),
        "# Loopback only: the reverse proxy is the only way in (spec section 1).".to_owned(),
        format!("ExecStart={}", exec_start(layout)),
        "Restart=on-failure".to_owned(),
        "RestartSec=5s".to_owned(),
        "# A stop signals every process of the service, so no agent or shell outlives it.".to_owned(),
        "KillMode=control-group".to_owned(),
        String::new(),
        "# Hardening that leaves agents able to build, test and reach the network.".to_owned(),
        "# No privilege gain through setuid binaries or file capabilities.".to_owned(),
        "NoNewPrivileges=yes".to_owned(),
        "# A private /tmp and /var/tmp, emptied when the service stops.".to_owned(),
        "PrivateTmp=yes".to_owned(),
        "# The whole file system is read-only except the paths below.".to_owned(),
        "ProtectSystem=strict".to_owned(),
        "# Calin's data, the repositories, and the home where agent CLIs keep their logins.".to_owned(),
        format!("ReadWritePaths={rw}"),
        "# No writes to /proc/sys and /sys, no kernel modules, a read-only cgroup tree.".to_owned(),
        "ProtectKernelTunables=yes".to_owned(),
        "ProtectKernelModules=yes".to_owned(),
        "ProtectControlGroups=yes".to_owned(),
        "# No new setuid or setgid files, no personality switches, no realtime scheduling.".to_owned(),
        "RestrictSUIDSGID=yes".to_owned(),
        "LockPersonality=yes".to_owned(),
        "RestrictRealtime=yes".to_owned(),
        "# Left out on purpose:".to_owned(),
        "# ProtectHome would hide the service user's home, where agents keep logins and caches.".to_owned(),
        "# PrivateNetwork would cut agents off from their APIs, git remotes and package registries.".to_owned(),
        "# MemoryDenyWriteExecute breaks JIT runtimes such as Node.js (which runs Claude Code) and the JVM.".to_owned(),
        "# A restrictive SystemCallFilter breaks compilers, package managers and test tools.".to_owned(),
        String::new(),
        "[Install]".to_owned(),
        "WantedBy=multi-user.target".to_owned(),
    ];
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// The environment file. The only rendered output that holds a secret.
pub fn render_env(input: &InstallInput) -> String {
    let mut out = String::new();
    out.push_str(
        "# Calin's public-mode settings (docs/specs/2026-11-27-remote-access.md, section 2),\n",
    );
    out.push_str("# read by systemd through EnvironmentFile= in calin.service.\n");
    if input.client_secret().is_some() {
        out.push_str(
            "# This file holds a secret, the OpenID Connect client secret: keep it owned by root\n",
        );
    } else {
        out.push_str("# This file is where the client secret goes when there is one: keep it owned by root\n");
    }
    out.push_str("# with mode 0600 and never share it.\n");
    let mut line = |key: &str, value: &str| {
        // Writing to a String cannot fail.
        let _ = writeln!(out, "{}", assignment(key, value));
    };
    line("CALIN_PUBLIC_URL", &input.public_url());
    line("CALIN_OIDC_ISSUER", input.issuer());
    line("CALIN_OIDC_CLIENT_ID", input.client_id());
    if let Some(secret) = input.client_secret() {
        line(SECRET_VARIABLE, secret.expose());
    }
    line("CALIN_ALLOWED_USERS", &input.allowed_users().join(","));
    out
}

/// The Caddy site, imported from the main Caddyfile.
pub fn render_caddy(domain: &str, layout: &ServiceLayout) -> String {
    let lines = [
        "# Calin, written by `calin server install`. Re-run it rather than editing this file."
            .to_owned(),
        format!("# The main Caddyfile loads it with: {CADDY_IMPORT_LINE}"),
        "#".to_owned(),
        "# Calin's Host and Origin guards and its rate limits rely on two things Caddy does by"
            .to_owned(),
        "# default: it passes the Host header through unchanged and it sets X-Forwarded-For."
            .to_owned(),
        "# Another proxy put in Caddy's place must do both, or Calin refuses its requests."
            .to_owned(),
        "".to_owned(),
        "# The site address alone gives automatic HTTPS: a certificate and a redirect from HTTP."
            .to_owned(),
        format!("{domain} {{"),
        "\t# Enough for a file saved from the editor; larger bodies are refused.".to_owned(),
        "\trequest_body {".to_owned(),
        format!("\t\tmax_size {MAX_BODY}"),
        "\t}".to_owned(),
        "\tencode zstd gzip".to_owned(),
        "\t# WebSocket upgrades pass through as they are. No cache directives for /api or /auth:"
            .to_owned(),
        "\t# Calin sets its own Cache-Control headers.".to_owned(),
        format!("\treverse_proxy {}", layout.bind()),
        "}".to_owned(),
    ];
    let mut out = String::new();
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// The unit's meaning without its comments and spacing, for drift checks.
pub(crate) fn normalize_unit(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with(';'))
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::input::InstallInputBuilder;

    pub(crate) fn typical() -> InstallInputBuilder {
        InstallInput::builder()
            .domain("calin.example.com")
            .issuer("https://example.eu.auth0.com/")
            .client_id("Xq3BfZ9kLmN0pRsT")
            .allowed_user("owner@example.com")
    }

    pub(crate) const AWKWARD_SECRET: &str = "s3cr\"et $HOME 'x' \\ #1";

    pub(crate) fn awkward() -> InstallInputBuilder {
        InstallInput::builder()
            .domain("agents.dev-box.example.org")
            .issuer("https://login.example.org:8443/realms/main")
            .client_id("calin client")
            .client_secret(AWKWARD_SECRET)
            .allowed_users(["Owner.Name+calin@example.org", "sub:auth0|5f1e2d3c"])
            .user("agents")
            .home_dir("/var/lib/agents-home")
            .data_dir("/srv/agents/data")
            .binary("/opt/calin/bin/calin")
            .port(18080)
            .extra_dirs(["/srv/agents/repos", "/opt/work"])
    }

    /// `Key=value` directives of the `[Service]` section, in order.
    fn directives(unit: &str) -> Vec<(String, String)> {
        unit.lines()
            .map(str::trim)
            .filter(|l| !l.starts_with('#') && !l.starts_with('['))
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
            .collect()
    }

    fn values<'a>(dirs: &'a [(String, String)], key: &str) -> Vec<&'a str> {
        dirs.iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    #[test]
    fn the_unit_has_the_hardening_and_nothing_that_breaks_agents() {
        for builder in [typical(), awkward()] {
            let input = builder.build().unwrap();
            let layout = input.layout();
            let unit = render_unit(layout);
            let dirs = directives(&unit);
            for (key, value) in [
                ("NoNewPrivileges", "yes"),
                ("PrivateTmp", "yes"),
                ("ProtectSystem", "strict"),
                ("ProtectKernelTunables", "yes"),
                ("ProtectKernelModules", "yes"),
                ("ProtectControlGroups", "yes"),
                ("RestrictSUIDSGID", "yes"),
                ("LockPersonality", "yes"),
                ("RestrictRealtime", "yes"),
                ("KillMode", "control-group"),
                ("Restart", "on-failure"),
                ("EnvironmentFile", ENV_PATH),
                ("After", "network-online.target"),
                ("Wants", "network-online.target"),
                ("WantedBy", "multi-user.target"),
            ] {
                assert_eq!(values(&dirs, key), [value], "{key}");
            }
            for forbidden in [
                "ProtectHome",
                "PrivateNetwork",
                "MemoryDenyWriteExecute",
                "SystemCallFilter",
                "PrivateUsers",
                "DynamicUser",
            ] {
                assert!(values(&dirs, forbidden).is_empty(), "{forbidden}");
            }
            assert_eq!(values(&dirs, "User"), [layout.user()]);
            assert_eq!(values(&dirs, "Group"), [layout.user()]);
            let exec = format!(
                "{} serve --bind 127.0.0.1:{} --home {}",
                layout.binary().display(),
                layout.port(),
                layout.data_dir().display()
            );
            assert_eq!(values(&dirs, "ExecStart"), [exec.as_str()]);
            let rw: Vec<&str> = values(&dirs, "ReadWritePaths")
                .iter()
                .flat_map(|v| v.split_whitespace())
                .collect();
            let mut expected = vec![layout.data_dir().to_str().unwrap()];
            expected.extend(layout.extra_dirs().iter().map(|p| p.to_str().unwrap()));
            expected.push(layout.home_dir().to_str().unwrap());
            for path in &expected {
                assert!(rw.contains(path), "{path} not in {rw:?}");
            }
            let restart_sec = values(&dirs, "RestartSec");
            assert_eq!(restart_sec.len(), 1);
            // The forbidden directives are explained in comments.
            for word in [
                "ProtectHome",
                "PrivateNetwork",
                "MemoryDenyWriteExecute",
                "SystemCallFilter",
            ] {
                assert!(
                    unit.lines().any(|l| l.starts_with('#') && l.contains(word)),
                    "{word} is not explained"
                );
            }
        }
    }

    #[test]
    fn the_caddy_site_proxies_to_the_loopback_port() {
        for builder in [typical(), awkward()] {
            let input = builder.build().unwrap();
            let caddy = render_caddy(input.domain(), input.layout());
            let code: Vec<&str> = caddy
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .collect();
            assert_eq!(
                code.first().copied(),
                Some(format!("{} {{", input.domain()).as_str())
            );
            assert_eq!(code.last().copied(), Some("}"));
            let upstream = format!("reverse_proxy 127.0.0.1:{}", input.layout().port());
            assert!(code.contains(&upstream.as_str()), "{caddy}");
            assert!(code.contains(&"max_size 10MB"), "{caddy}");
            assert!(code.contains(&"encode zstd gzip"), "{caddy}");
            assert!(!code.iter().any(|l| l.contains("Cache-Control")), "{caddy}");
            assert!(!code.iter().any(|l| l.starts_with("header")), "{caddy}");
            let opens = caddy.matches('{').count();
            assert_eq!(opens, caddy.matches('}').count());
            assert!(caddy.contains("X-Forwarded-For") && caddy.contains("Host"));
        }
    }

    #[test]
    fn the_env_file_reads_back_through_systemds_syntax() {
        let input = awkward().build().unwrap();
        let env = render_env(&input);
        let parsed = crate::envfile::parse_env_file(&env).unwrap();
        let expected = [
            ("CALIN_PUBLIC_URL", "https://agents.dev-box.example.org"),
            (
                "CALIN_OIDC_ISSUER",
                "https://login.example.org:8443/realms/main",
            ),
            ("CALIN_OIDC_CLIENT_ID", "calin client"),
            ("CALIN_OIDC_CLIENT_SECRET", AWKWARD_SECRET),
            (
                "CALIN_ALLOWED_USERS",
                "Owner.Name+calin@example.org,sub:auth0|5f1e2d3c",
            ),
        ];
        let expected: Vec<(String, String)> = expected
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        assert_eq!(parsed, expected);
        assert!(env.contains("holds a secret"));
    }

    #[test]
    fn without_a_secret_the_variable_is_omitted() {
        let env = render_env(&typical().build().unwrap());
        assert!(!env.contains(SECRET_VARIABLE));
        let parsed = crate::envfile::parse_env_file(&env).unwrap();
        let keys: Vec<&str> = parsed.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, REQUIRED_VARIABLES);
    }

    #[test]
    fn rendering_is_deterministic() {
        let a = awkward().build().unwrap();
        let b = awkward().build().unwrap();
        assert_eq!(render_unit(a.layout()), render_unit(b.layout()));
        assert_eq!(render_env(&a), render_env(&b));
        assert_eq!(
            render_caddy(a.domain(), a.layout()),
            render_caddy(b.domain(), b.layout())
        );
    }

    #[test]
    fn normalizing_ignores_comments_and_spacing_only() {
        let unit = render_unit(typical().build().unwrap().layout());
        let reformatted: String = unit
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(|l| format!("  {}   \n\n", l.replace(' ', "  ")))
            .collect();
        assert_eq!(normalize_unit(&unit), normalize_unit(&reformatted));
        let changed = unit.replace("Restart=on-failure", "Restart=always");
        assert_ne!(normalize_unit(&unit), normalize_unit(&changed));
    }

    /// `systemd-analyze verify` on the rendered unit, with a binary that
    /// exists here. Skipped when the tool is absent.
    #[test]
    fn systemd_analyze_accepts_the_unit() {
        let Ok(probe) = std::process::Command::new("systemd-analyze")
            .arg("--version")
            .output()
        else {
            eprintln!("note: systemd-analyze not found; skipping the verify test");
            return;
        };
        if !probe.status.success() {
            eprintln!("note: systemd-analyze does not run here; skipping the verify test");
            return;
        }
        for builder in [typical(), awkward()] {
            let input = builder.binary("/usr/bin/true").build().unwrap();
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(UNIT_NAME);
            std::fs::write(&path, render_unit(input.layout())).unwrap();
            let output = std::process::Command::new("systemd-analyze")
                .args(["verify", "--man=no"])
                .arg(&path)
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            // The service user, its home and the env file do not exist here.
            // It exits 0 even on warnings, so any line is a complaint, except
            // that the service user does not exist on this machine.
            let unexpected: Vec<&str> = stderr
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter(|l| {
                    let lower = l.to_lowercase();
                    !(lower.contains("user")
                        && !lower.contains("failed to parse")
                        && !lower.contains("unknown"))
                })
                .collect();
            assert!(
                output.status.success() && unexpected.is_empty(),
                "systemd-analyze verify complained:\n{stderr}"
            );
        }
    }
}
