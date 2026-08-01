# Security policy

## Reporting a vulnerability

Please do not open a public issue for a suspected vulnerability. Use GitHub's
private vulnerability reporting for `kevinelliott/arrman` so credentials,
configuration paths, and reproduction details are not disclosed before a fix is
available.

Include the affected version, deployment mode, impact, minimal reproduction, and
whether the issue involves Docker socket access, web authentication, setup
imports, archive extraction, command execution, integration redaction, or backup
contents.

## Deployment boundaries

Arrman is an administrative tool. Access to the Docker socket is effectively
host-level control. Keep the web listener on loopback or behind an authenticated
TLS reverse proxy, protect token and setup-bundle files with restrictive
permissions, and do not run an untrusted image or Compose file through Arrman.

Setup bundles can contain application credentials even though media and download
content is excluded. Encrypt and access-control them according to the sensitivity
of the managed stack.

Only the latest release receives security fixes during the initial release line.
