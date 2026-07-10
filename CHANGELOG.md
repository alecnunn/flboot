# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/alecnunn/flboot/compare/v0.1.3...v0.2.0) - 2026-07-10

### Added

- [**breaking**] run diff, dis and progress in-process via objdiff-core ([#19](https://github.com/alecnunn/flboot/pull/19))

## [0.1.3](https://github.com/alecnunn/flboot/compare/v0.1.2...v0.1.3) - 2026-07-09

### Fixed

- *(codegen)* merge cflags into cxxflags per-unit, fixing dropped /GX and /G6 ([#17](https://github.com/alecnunn/flboot/pull/17))

## [0.1.2](https://github.com/alecnunn/flboot/compare/v0.1.1...v0.1.2) - 2026-07-09

### Fixed

- *(fetch)* decode BCJ-filtered xz archives, fixing 7-Zip on Linux ([#11](https://github.com/alecnunn/flboot/pull/11))

## [0.1.1](https://github.com/alecnunn/flboot/compare/v0.1.0...v0.1.1) - 2026-07-05

### Added

- diff all functions when symbol omitted; accept object-file names ([#6](https://github.com/alecnunn/flboot/pull/6))

### Other

- enable release-plz git_only so versions bump from git tags
