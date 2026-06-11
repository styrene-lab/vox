# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.5](https://github.com/styrene-lab/vox/compare/vox-v0.1.4...vox-v0.1.5) - 2026-06-11

### Added

- *(vox)* accept bootstrap runtime config
- overnight reviewer agent, helm chart, OCI build pipeline
- slack proxy mode for operator channel monitoring
- discord-agent container and rustls TLS backend
- complete communication connectors and extension RPC framework

### Fixed

- match ToolDefinition schema (add label, rename input_schema to parameters)
- bump omegon-extension to 0.15.24 (numeric RPC id fix)
- exclude vox-lxmf from workspace until styrene-rs is published
- add exponential backoff to bridge mode daemon push

### Other

- *(release)* bump vox to 0.1.4
- bump vox to 0.1.1
- switch omegon-extension to crates.io dependency
- Initial scaffold: core abstractions and omegon extension binary
