# Supported services

Arrman discovers every valid service in the selected Docker Compose model. The
catalog below is the current first-class metadata set: these names receive a
specific purpose instead of the generic “Docker container” label, and some also
participate in dependency ownership or integration reconciliation.

Health-check definitions, ordinary `depends_on` edges, published ports, mounts,
URLs, runtime state, and container health are always read from Compose and Docker
at runtime. A service does not need to appear in this table to receive those core
capabilities.

| Compose service | Purpose | Arrman support | Upstream |
| --- | --- | --- | --- |
| `audiobookshelf` | Audiobook and podcast server | Catalog | [advplyr/audiobookshelf](https://github.com/advplyr/audiobookshelf) |
| `autopulse` | Media automation and monitoring | Catalog + owned dependency | [dan-online/autopulse](https://github.com/dan-online/autopulse) |
| `autopulse-postgres` | Autopulse database dependency | Owned dependency | [PostgreSQL](https://www.postgresql.org/) |
| `bazarr` | Subtitle manager | Catalog + Scraparr connector | [morpheus65535/bazarr](https://github.com/morpheus65535/bazarr) |
| `decluttarr` | Cleans stalled and failed downloads | Catalog | [ManiMatter/decluttarr](https://github.com/ManiMatter/decluttarr) |
| `flaresolverr` | Cloudflare challenge proxy | Catalog | [FlareSolverr/FlareSolverr](https://github.com/FlareSolverr/FlareSolverr) |
| `floppy` | Self-hosted media tracking | Catalog + owned dependency | [dannyvfilms/Floppy](https://github.com/dannyvfilms/Floppy) |
| `floppy-redis` | Floppy cache dependency | Owned dependency | [Redis](https://github.com/redis/redis) |
| `homarr` | Media-server dashboard | Catalog | [homarr-labs/homarr](https://github.com/homarr-labs/homarr) |
| `jellyfin` | Media server and streaming | Catalog + Scraparr and Tracearr integrations | [jellyfin/jellyfin](https://github.com/jellyfin/jellyfin) |
| `jellyseerr` | Jellyfin media request manager | Catalog; legacy service name for Seerr deployments | [seerr-team/seerr](https://github.com/seerr-team/seerr) |
| `kapowarr` | Comic library manager | Catalog | [Casvt/Kapowarr](https://github.com/Casvt/Kapowarr) |
| `lidarr` | Music library manager | Catalog + Scraparr connector | [Lidarr/Lidarr](https://github.com/Lidarr/Lidarr) |
| `maintainerr` | Media library maintenance and cleanup | Catalog | [Maintainerr/Maintainerr](https://github.com/Maintainerr/Maintainerr) |
| `posterizarr` | Automated media poster and artwork generator | Catalog | [fscorrupt/Posterizarr](https://github.com/fscorrupt/Posterizarr) |
| `prefetcharr` | On-demand episode prefetcher for active playback | Catalog | [p-hueber/prefetcharr](https://github.com/p-hueber/prefetcharr) |
| `prowlarr` | Indexer manager and proxy | Catalog + application-link reconciliation + Scraparr connector | [Prowlarr/Prowlarr](https://github.com/Prowlarr/Prowlarr) |
| `pulsarr` | Plex watchlist and request manager | Catalog | [jamcalli/Pulsarr](https://github.com/jamcalli/Pulsarr) |
| `radarr` | Movie library manager | Catalog + SABnzbd, Prowlarr, and Scraparr integrations | [Radarr/Radarr](https://github.com/Radarr/Radarr) |
| `sabnzbd` | Usenet downloader | Catalog + allow-list and download-client reconciliation | [sabnzbd/sabnzbd](https://github.com/sabnzbd/sabnzbd) |
| `scraparr` | Prometheus metrics exporter for media services | Catalog + connector reconciliation | [thecfu/scraparr](https://github.com/thecfu/scraparr) |
| `seerr` | Media request and discovery manager | Catalog | [seerr-team/seerr](https://github.com/seerr-team/seerr) |
| `sonarr` | TV series library manager | Catalog + SABnzbd, Prowlarr, and Scraparr integrations | [Sonarr/Sonarr](https://github.com/Sonarr/Sonarr) |
| `spotweb` | Spotweb Usenet indexer and Newznab API | Catalog + owned dependency | [spotweb/spotweb](https://github.com/spotweb/spotweb) · [edv/docker-spotweb](https://github.com/edv/docker-spotweb) |
| `spotweb-mysql` | Spotweb database dependency | Owned dependency | [MySQL](https://github.com/mysql/mysql-server) |
| `tdarr` | Media transcoding and processing | Catalog | [HaveAGitGat/Tdarr](https://github.com/HaveAGitGat/Tdarr) |
| `tdarr-node` | Tdarr transcoding worker | Catalog worker | [HaveAGitGat/Tdarr](https://github.com/HaveAGitGat/Tdarr) |
| `tracearr` | Real-time media playback monitoring and analytics | Catalog + owned dependencies + Jellyfin integration | [connorgallopo/Tracearr](https://github.com/connorgallopo/Tracearr) |
| `tracearr-redis` | Tracearr cache and job queue dependency | Owned dependency | [Redis](https://github.com/redis/redis) |
| `tracearr-timescale` | Tracearr time-series database dependency | Owned dependency | [TimescaleDB](https://github.com/timescale/timescaledb) |
| `trailarr` | Trailer downloader and manager | Catalog | [nandyalu/trailarr](https://github.com/nandyalu/trailarr) |
| `unpackarr` | Download extraction helper | Catalog | [Unpackerr](https://github.com/Unpackerr/unpackerr) |
| `watcharr` | Watch tracking and lists | Catalog | [sbondCo/Watcharr](https://github.com/sbondCo/Watcharr) |
| `wizarr` | Jellyfin invite and onboarding | Catalog | [wizarrrr/wizarr](https://github.com/wizarrrr/wizarr) |

## Owned dependency groups

Owned dependencies are stopped with their application only when Arrman can prove
they are exclusive to that service. Ordinary shared dependencies remain up.

| Application | Owned services |
| --- | --- |
| `autopulse` | `autopulse-postgres` |
| `floppy` | `floppy-redis` |
| `spotweb` | `spotweb-mysql` |
| `tracearr` | `tracearr-redis`, `tracearr-timescale` |

Tdarr Node is cataloged as a worker, but its lifecycle follows the actual
Compose graph rather than a hard-coded exclusive relationship.

## Integration paths

Integration plans are generated only when the required services are enabled and
reachable. They are safe to recalculate after enablement or port changes.

- SABnzbd host allow-list and Sonarr/Radarr download clients.
- Prowlarr application links to Sonarr and Radarr.
- Scraparr connector configuration for Sonarr, Radarr, Prowlarr, Bazarr, Lidarr,
  and Jellyfin.
- Tracearr connection metadata for Jellyfin.

Plans preserve unknown API fields and redact credentials from serialized state,
logs, and errors. When an API cannot be configured safely, Arrman reports a
manual action instead of guessing.

## Add a service

See [CONTRIBUTING.md](../CONTRIBUTING.md#adding-service-support). The catalog and
this table are checked together in CI, so a new built-in service cannot be
merged with stale documentation.
