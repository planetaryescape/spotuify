# Changelog

## [0.1.4](https://github.com/planetaryescape/spotuify/compare/v0.1.3...v0.1.4) (2026-05-31)


### Features

* choose the embedded player's local audio output device ([c208642](https://github.com/planetaryescape/spotuify/commit/c208642b1a49bfbcc6bb44fdd8029b9fbe6dd402))
* clearer error when transferring to an Alexa/Echo device ([1236002](https://github.com/planetaryescape/spotuify/commit/1236002e7befc92ac0c3a57c1dd74b77eb27bf54))
* don't auto-restart a stale daemon mid-playback ([1cc4595](https://github.com/planetaryescape/spotuify/commit/1cc4595e1e862801d60d7612e5b6e94b3303df2e))
* first-party Spotify login (keymaster + login5) ([dd86966](https://github.com/planetaryescape/spotuify/commit/dd869665462d7a804e352a5fd5af8327a1eb6c2b))
* make TUI home actionable ([1f793c5](https://github.com/planetaryescape/spotuify/commit/1f793c5af284cbfa181f0d4f2cf161d00a76e207))
* show playlist and album artwork in tui ([10ff578](https://github.com/planetaryescape/spotuify/commit/10ff57865c2f14fc44ffa5883586e7383241a7d5))
* spotuify playlist set-image (custom cover art) ([1c50241](https://github.com/planetaryescape/spotuify/commit/1c50241ad1094e7113041da0f50b88fec940a6f7))
* teleprompter lyrics + softer queue selection highlight ([07389ca](https://github.com/planetaryescape/spotuify/commit/07389ca438f72488c83cb761b04a3cb8b2ec48d0))
* TUI banner to restart the daemon after an in-place upgrade ([09b349d](https://github.com/planetaryescape/spotuify/commit/09b349dad1a44fa4d79e08447554ca7d682364dc))
* TUI picker for the local audio output device ([19fcb46](https://github.com/planetaryescape/spotuify/commit/19fcb462c878d872c0613eac3dceb150bc6d0345))


### Bug Fixes

* activate embedded device so volume and transport work ([2a6dd17](https://github.com/planetaryescape/spotuify/commit/2a6dd179dcef5f459f6f840e2786f2a5804f8a26))
* auto-play first selection when queueing with no active device ([e22fa2f](https://github.com/planetaryescape/spotuify/commit/e22fa2fee0a800c363f9667c85e4ecd4616d14f7))
* avoid playback rate-limit preflights ([da05376](https://github.com/planetaryescape/spotuify/commit/da05376f44a0c7354f1af9dc605905a2b9222938))
* bound provider rate-limit waits ([8c6f6f7](https://github.com/planetaryescape/spotuify/commit/8c6f6f7f7abcbfbd51534bbc989693b187a055d5))
* call POST /me/playlists instead of POST /users/{user_id}/playlists ([3587625](https://github.com/planetaryescape/spotuify/commit/3587625cce7fb59de36594d48467b66d212b9188))
* consolidate Spotify endpoints; route playlist writes to /items ([f79917f](https://github.com/planetaryescape/spotuify/commit/f79917f178c4be17a4586c4ff20a6b4ef6f08e1d))
* declare Homebrew portaudio dependency ([404343e](https://github.com/planetaryescape/spotuify/commit/404343eea09614b249e5e1c57f35017689730bb6))
* escalating global backoff so the daemon self-heals from throttling ([b6d93a8](https://github.com/planetaryescape/spotuify/commit/b6d93a8c755aeeab09cad7aaec61437c233da9f4))
* harden first-party auth (review findings) ([0d6fc7a](https://github.com/planetaryescape/spotuify/commit/0d6fc7a4071706177138afa7aef3d96a690f86ff))
* harden public release security ([f2058e5](https://github.com/planetaryescape/spotuify/commit/f2058e565b8466412fb3a113c15946ec2c767b02))
* isolate target builds from prod daemon ([6bf348a](https://github.com/planetaryescape/spotuify/commit/6bf348ac05139f35efc8dcf4a1483ac997f394b4))
* load docs fonts without blocking render ([e8a87a9](https://github.com/planetaryescape/spotuify/commit/e8a87a9014e2f8f09c1bf7c0c413c88b0a0293d5))
* poll devices and queue on slower lanes than playback ([10f4c84](https://github.com/planetaryescape/spotuify/commit/10f4c8436f27214735fbd3e428eabc331182c604))
* preserve playback context and audio defaults ([98f25bd](https://github.com/planetaryescape/spotuify/commit/98f25bde8097e34d3fd70e045f24c06688d9c8b7))
* record operations log + add playlist unfollow + auth bearer ([21c8635](https://github.com/planetaryescape/spotuify/commit/21c86357534ceb39c5325d636c457e32d1d539be))
* recover onboarding from blank config ([d45cb56](https://github.com/planetaryescape/spotuify/commit/d45cb56996576bb1744c085b8dd14873f774146d))
* stop auth prompt storms ([478f634](https://github.com/planetaryescape/spotuify/commit/478f63443c4372bcbf33ad05b80fc5a1bf2dbf6c))
* stop chronic Web API rate-limiting under first-party auth ([a34aa06](https://github.com/planetaryescape/spotuify/commit/a34aa060853daa450b4b52cdcfcc04e8e5c4bae3))
* stop startup cache-warm bursts that re-feed the rate limit ([25988bd](https://github.com/planetaryescape/spotuify/commit/25988bdc15cd80f81e41810d76bf8418e71967c3))
* unstick lyrics "Fetching" on track-change race ([67026cc](https://github.com/planetaryescape/spotuify/commit/67026cca1d862a69cc44675d38f806b37086513c))


### Refactoring

* simplify workspace and harden daemon accept loop ([4119e70](https://github.com/planetaryescape/spotuify/commit/4119e70b49650c7609e1bff93263065857d189a8))


### Documentation

* add demo media and agent skill with mcp integration ([3c6e22b](https://github.com/planetaryescape/spotuify/commit/3c6e22bc198d9cd465f9d9751ac9420fd90b9af2))
* add idiomatic-rust rubric and backlog ([11593b2](https://github.com/planetaryescape/spotuify/commit/11593b2f2ae8ad484ed9ad9f9f18e0a42a424c81))
* add player screenshot asset ([7b00876](https://github.com/planetaryescape/spotuify/commit/7b00876cff785ca5af0d5c4477ba1661ded70fba))
* add security audit v2 ([a3ad5f0](https://github.com/planetaryescape/spotuify/commit/a3ad5f033088489979c62a2fc0dbea47c8769805))
* correct remaining dev-app onboarding references ([e0294b4](https://github.com/planetaryescape/spotuify/commit/e0294b4ffb60accac34b07b7ed18363646eb8dcd))
* document audio-output, audio-outputs, and search-page commands ([f8eb793](https://github.com/planetaryescape/spotuify/commit/f8eb793127a5778b9c494b85a22c9cb2baf4e8b4))
* first-party login onboarding ([bd6a919](https://github.com/planetaryescape/spotuify/commit/bd6a919d9028b0bae379afbc1e7d38db316ffab8))
* fix Homebrew install commands ([70b8ed4](https://github.com/planetaryescape/spotuify/commit/70b8ed45cfa1df66d2ad7e20637f2badbce0f833))
* lead README with plain positioning and honest install matrix ([c7783bd](https://github.com/planetaryescape/spotuify/commit/c7783bdfc24a2b7635b420c46ddb0a83fae8f0df))
* refresh guides + reference for recent shipped changes ([a525922](https://github.com/planetaryescape/spotuify/commit/a5259223a66f4bb620f6cd75ab6c6466492178b2))
* serve demo gif from site/public for a stable URL ([af7cc32](https://github.com/planetaryescape/spotuify/commit/af7cc329ed64b3c0e57a1ef5f845124febd1efa7))
* sharpen landing positioning (name prior art, lead pipeable CLI) ([17f2a08](https://github.com/planetaryescape/spotuify/commit/17f2a08185a4d49d5425d10a8ec1eba1649c37ec))
* widen landing demo ([a252a0f](https://github.com/planetaryescape/spotuify/commit/a252a0f933c8bfcabd3b65c0f24a5decffee12bd))
