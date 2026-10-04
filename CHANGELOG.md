# Changelog

## [0.1.11](https://github.com/the-mentor/cbox/compare/v0.1.10...v0.1.11) (2026-10-04)


### Bug Fixes

* **cbox:** stop the hookfwd drain test racing on ETXTBSY ([#85](https://github.com/the-mentor/cbox/issues/85)) ([936e23b](https://github.com/the-mentor/cbox/commit/936e23b9647d11a8aa45d01293d181f08c3a5e87))

## [0.1.10](https://github.com/the-mentor/cbox/compare/v0.1.9...v0.1.10) (2026-10-04)


### Bug Fixes

* **cbox:** renew an expiring MITM CA when a box starts ([#82](https://github.com/the-mentor/cbox/issues/82)) ([481e7ae](https://github.com/the-mentor/cbox/commit/481e7aec5615e44de024789b2aad8ef852e80848)), closes [#81](https://github.com/the-mentor/cbox/issues/81)

## [0.1.9](https://github.com/the-mentor/cbox/compare/v0.1.8...v0.1.9) (2026-09-30)


### Performance Improvements

* **base:** drop build caches and share oh-my-posh themes ([#79](https://github.com/the-mentor/cbox/issues/79)) ([c4ec9a1](https://github.com/the-mentor/cbox/commit/c4ec9a12c9476cfd8d2ab6cb3425c812d8472d74))

## [0.1.8](https://github.com/the-mentor/cbox/compare/v0.1.7...v0.1.8) (2026-09-30)


### Features

* **base:** bump uv to 0.12.21 and install pre-commit ([#77](https://github.com/the-mentor/cbox/issues/77)) ([91d5e7c](https://github.com/the-mentor/cbox/commit/91d5e7cda2d48a36fa51f33051647d408cda56ba))

## [0.1.7](https://github.com/the-mentor/cbox/compare/v0.1.6...v0.1.7) (2026-09-30)


### Bug Fixes

* **cbox:** sweep unused disk images on up --force ([#74](https://github.com/the-mentor/cbox/issues/74)) ([ea4e05b](https://github.com/the-mentor/cbox/commit/ea4e05b1f470a47b1d5d85ab58275c3d954ab4f8))

## [0.1.6](https://github.com/the-mentor/cbox/compare/v0.1.5...v0.1.6) (2026-09-29)


### Bug Fixes

* **agentgateway:** allow the admin UI playgrounds through CORS ([#72](https://github.com/the-mentor/cbox/issues/72)) ([492a94b](https://github.com/the-mentor/cbox/commit/492a94b4e9db5147cd22c5b76264156a32475005))

## [0.1.5](https://github.com/the-mentor/cbox/compare/v0.1.4...v0.1.5) (2026-09-28)


### Bug Fixes

* **cbox:** delete the box home on down ([#69](https://github.com/the-mentor/cbox/issues/69)) ([3ad5a31](https://github.com/the-mentor/cbox/commit/3ad5a31c5d3ba0cd66fc765ba325c62679bad601))

## [0.1.4](https://github.com/the-mentor/cbox/compare/v0.1.3...v0.1.4) (2026-09-28)


### Features

* **ci:** build and publish the cbox-base image ([#67](https://github.com/the-mentor/cbox/issues/67)) ([d2ae43a](https://github.com/the-mentor/cbox/commit/d2ae43a25549ffed5d1e708ed9cd673892610ba8))

## [0.1.3](https://github.com/the-mentor/cbox/compare/v0.1.2...v0.1.3) (2026-09-27)


### Features

* **image:** bake the no-ai-attribution plugin into the box ([#65](https://github.com/the-mentor/cbox/issues/65)) ([c9d0a3e](https://github.com/the-mentor/cbox/commit/c9d0a3e1f7deb6fbcc20d709bbc65532e0bf6df0))

## [0.1.2](https://github.com/the-mentor/cbox/compare/v0.1.1...v0.1.2) (2026-09-27)


### Features

* **justfile:** add just version to print the installed cbox version ([#63](https://github.com/the-mentor/cbox/issues/63)) ([c372f64](https://github.com/the-mentor/cbox/commit/c372f64d54ccfe333cb4872cf2e219aa96050956))

## [0.1.1](https://github.com/the-mentor/cbox/compare/v0.1.0...v0.1.1) (2026-09-27)


### Bug Fixes

* **ci:** always build on release PR merges so releases get binaries ([#60](https://github.com/the-mentor/cbox/issues/60)) ([bc1f377](https://github.com/the-mentor/cbox/commit/bc1f3771ff2aa2bf00f9c1257fbbf87ef3f03f86))

## [0.1.0](https://github.com/the-mentor/cbox/compare/v0.1.0...v0.1.0) (2026-09-27)


### Continuous Integration

* run release after ci passes on main, cut the first release ([#58](https://github.com/the-mentor/cbox/issues/58)) ([8b5f8d4](https://github.com/the-mentor/cbox/commit/8b5f8d40f4d7868edaca68f261c208501d286c78))
