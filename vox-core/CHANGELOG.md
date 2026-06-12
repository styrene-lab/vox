# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.6](https://github.com/styrene-lab/vox/compare/vox-core-v0.1.5...vox-core-v0.1.6) - 2026-06-12

### Added

- support connector token files
- overnight reviewer agent, helm chart, OCI build pipeline
- slack proxy mode for operator channel monitoring
- group-based operator trust for Discord roles and Slack usergroups
- role-based operator trust via Discord server roles
- trust-level access control — separate instruction plane from data plane
- complete communication connectors and extension RPC framework

### Fixed

- *(core)* tolerate poisoned secret store locks
- *(core)* reject broadly readable secret files
- *(voice)* add Display for engines, handle optional TTS model path

### Other

- *(release)* bump vox to 0.1.5
- *(core)* satisfy clippy for default enums
- *(release)* bump vox to 0.1.4
- bump vox to 0.1.1
- Initial scaffold: core abstractions and omegon extension binary
