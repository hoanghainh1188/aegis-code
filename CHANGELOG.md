# Changelog

## [0.5.0](https://github.com/hoanghainh1188/aegis-code/compare/v0.4.0...v0.5.0) (2026-11-02)


### Features

* **aci:** redact secrets by entropy and protect sensitive files ([#360](https://github.com/hoanghainh1188/aegis-code/issues/360)) ([#394](https://github.com/hoanghainh1188/aegis-code/issues/394)) ([36d33dc](https://github.com/hoanghainh1188/aegis-code/commit/36d33dcc198f88eccfeedd3f116a621de79527ae))

## [0.4.0](https://github.com/hoanghainh1188/aegis-code/compare/v0.3.0...v0.4.0) (2026-11-01)


### Features

* **gate:** patch scope check and reviewer flags ([#359](https://github.com/hoanghainh1188/aegis-code/issues/359)) ([#393](https://github.com/hoanghainh1188/aegis-code/issues/393)) ([43b03ef](https://github.com/hoanghainh1188/aegis-code/commit/43b03ef05342e7abe7146ba30783cd79bf092484))
* pause a repo from its admin page or `repo pause`, its jobs kept ([#390](https://github.com/hoanghainh1188/aegis-code/issues/390)) ([cacacfd](https://github.com/hoanghainh1188/aegis-code/commit/cacacfdfd52f9a52a8ac97338be110d33ebfffef))
* **server:** author trust policy for labeled issues ([#358](https://github.com/hoanghainh1188/aegis-code/issues/358)) ([#392](https://github.com/hoanghainh1188/aegis-code/issues/392)) ([34efa69](https://github.com/hoanghainh1188/aegis-code/commit/34efa699077911aeb73919884682aedd6e468a5c))
* **web:** a repo's page shows what became of its recent webhooks ([#387](https://github.com/hoanghainh1188/aegis-code/issues/387)) ([fc67735](https://github.com/hoanghainh1188/aegis-code/commit/fc677359ffbca329993f09f36def35bd5e273843)), closes [#373](https://github.com/hoanghainh1188/aegis-code/issues/373)
* **web:** an admin makes a repo's webhooks with a token given once ([#391](https://github.com/hoanghainh1188/aegis-code/issues/391)) ([5737900](https://github.com/hoanghainh1188/aegis-code/commit/573790068d24a82175cd88f028c4c623f46f37f8))
* **web:** maintainers and admins change a repo's monthly cap from the pages ([#386](https://github.com/hoanghainh1188/aegis-code/issues/386)) ([615c553](https://github.com/hoanghainh1188/aegis-code/commit/615c553c77a8d307b3874e131c812ae14750b0da)), closes [#372](https://github.com/hoanghainh1188/aegis-code/issues/372)


### Bug Fixes

* **web:** masthead wraps on narrow screens so every link stays reachable ([#383](https://github.com/hoanghainh1188/aegis-code/issues/383)) ([827a36f](https://github.com/hoanghainh1188/aegis-code/commit/827a36fecaead04a6f584e276bd078194f170cc7)), closes [#365](https://github.com/hoanghainh1188/aegis-code/issues/365)
* **web:** pages link the stylesheet by its hash so an upgrade shows new CSS ([#385](https://github.com/hoanghainh1188/aegis-code/issues/385)) ([11b0adf](https://github.com/hoanghainh1188/aegis-code/commit/11b0adf0d147213c5ba69d1137354184044e3ab8)), closes [#375](https://github.com/hoanghainh1188/aegis-code/issues/375)
* **web:** the recent webhooks table reads across rows on a phone ([#389](https://github.com/hoanghainh1188/aegis-code/issues/389)) ([47f74b7](https://github.com/hoanghainh1188/aegis-code/commit/47f74b7bbad48f3b12c858e4db0626524141d81b)), closes [#388](https://github.com/hoanghainh1188/aegis-code/issues/388)

## [0.3.0](https://github.com/hoanghainh1188/aegis-code/compare/v0.2.4...v0.3.0) (2026-11-01)


### Features

* **web:** admin adds and changes repos, the clone made in the background ([#364](https://github.com/hoanghainh1188/aegis-code/issues/364)) ([4cac150](https://github.com/hoanghainh1188/aegis-code/commit/4cac1502ec9a21907f333b312c6b58979a876f3c))
* **web:** admin adds services and makes and revokes their API tokens ([#381](https://github.com/hoanghainh1188/aegis-code/issues/381)) ([b0adf36](https://github.com/hoanghainh1188/aegis-code/commit/b0adf36e9a52ee8dc1365f5e9fb7d0cc0405d4c6))
* **web:** admin manages users and the admin right ([#379](https://github.com/hoanghainh1188/aegis-code/issues/379)) ([27f6725](https://github.com/hoanghainh1188/aegis-code/commit/27f6725c143e342c1e729d2eb5c7f3fc70f5d457))
* **web:** admin reads the audit log, filtered by repo, who and action ([#378](https://github.com/hoanghainh1188/aegis-code/issues/378)) ([2a04c57](https://github.com/hoanghainh1188/aegis-code/commit/2a04c579dd1d301a5fd98cdb9a9d664d55ee71ab))
* **web:** admin sees and checks a repo's connection to its forge ([#377](https://github.com/hoanghainh1188/aegis-code/issues/377)) ([d0cad57](https://github.com/hoanghainh1188/aegis-code/commit/d0cad5721ec3ed5eea85b05462f0191d0746f000))
* **web:** admin sees each mode's AI and checks its connection ([#382](https://github.com/hoanghainh1188/aegis-code/issues/382)) ([3d0bd60](https://github.com/hoanghainh1188/aegis-code/commit/3d0bd60fd5bf87b7610952a7529377c1f8847a68))
* **web:** admin signs a person out everywhere ([#380](https://github.com/hoanghainh1188/aegis-code/issues/380)) ([fee16c9](https://github.com/hoanghainh1188/aegis-code/commit/fee16c9059ef5017c918a684ddb6a5c2132da82a))


### Bug Fixes

* **gate:** CI, CODEOWNERS and check configuration are protected files ([#352](https://github.com/hoanghainh1188/aegis-code/issues/352)) ([#354](https://github.com/hoanghainh1188/aegis-code/issues/354)) ([bffb13b](https://github.com/hoanghainh1188/aegis-code/commit/bffb13ba5b7377e363efa2743a79c378410a8210))
* **lifecycle:** untrusted text cannot write the platform's prompt tags ([#351](https://github.com/hoanghainh1188/aegis-code/issues/351)) ([#353](https://github.com/hoanghainh1188/aegis-code/issues/353)) ([56c21b5](https://github.com/hoanghainh1188/aegis-code/commit/56c21b5d59fe93c9bed57c09702608a51db6e9d1))
* **pr:** model text in a PR cannot make the reader fetch a URL ([#357](https://github.com/hoanghainh1188/aegis-code/issues/357)) ([#362](https://github.com/hoanghainh1188/aegis-code/issues/362)) ([5201b17](https://github.com/hoanghainh1188/aegis-code/commit/5201b179c3dbc96c1edbe68c7e2222503297a3ff))


### Documentation

* ADR 0009 prompt injection threat model ([#356](https://github.com/hoanghainh1188/aegis-code/issues/356)) ([#363](https://github.com/hoanghainh1188/aegis-code/issues/363)) ([59889e1](https://github.com/hoanghainh1188/aegis-code/commit/59889e140a530b163cbc957beb3311f97f013e9f))
* **npm:** enrich @hoanghainh/aegis package metadata, README, and idempotent publish ([#366](https://github.com/hoanghainh1188/aegis-code/issues/366)) ([afe1685](https://github.com/hoanghainh1188/aegis-code/commit/afe16853ac01eb6687b82ca69fd53104fd063c89))

## [0.2.4](https://github.com/hoanghainh1188/aegis-code/compare/v0.2.3...v0.2.4) (2026-11-01)


### Bug Fixes

* **store:** a store error rolls its transaction back before returning ([#349](https://github.com/hoanghainh1188/aegis-code/issues/349)) ([a4ac1f7](https://github.com/hoanghainh1188/aegis-code/commit/a4ac1f7648fc46b0a21ff9d170919beded67f1d2))

## [0.2.3](https://github.com/hoanghainh1188/aegis-code/compare/v0.2.2...v0.2.3) (2026-11-01)


### Documentation

* install from the published release; npm-release environment ([#347](https://github.com/hoanghainh1188/aegis-code/issues/347)) ([ac3f445](https://github.com/hoanghainh1188/aegis-code/commit/ac3f445bd6aa94934bfd341883355a610642e06d))

## [0.2.2](https://github.com/hoanghainh1188/aegis-code/compare/v0.2.1...v0.2.2) (2026-11-01)


### Bug Fixes

* **npm:** publish the client as @hoanghainh/aegis ([#342](https://github.com/hoanghainh1188/aegis-code/issues/342)) ([ab67808](https://github.com/hoanghainh1188/aegis-code/commit/ab678082fe54842714081d6d2a9033a35c32ec79))

## [0.2.1](https://github.com/hoanghainh1188/aegis-code/compare/v0.2.0...v0.2.1) (2026-10-31)


### Code Refactoring

* rename the client CLI from aegis-job to aegis ([#337](https://github.com/hoanghainh1188/aegis-code/issues/337)) ([957d95b](https://github.com/hoanghainh1188/aegis-code/commit/957d95b245ae93f4d9c47c85037359ae659bd14a))

## [0.2.0](https://github.com/hoanghainh1188/aegis-code/compare/v0.1.0...v0.2.0) (2026-10-31)


### Features

* **deploy:** on-premise with Docker Compose or systemd ([#262](https://github.com/hoanghainh1188/aegis-code/issues/262)) ([#277](https://github.com/hoanghainh1188/aegis-code/issues/277)) ([76d5cf7](https://github.com/hoanghainh1188/aegis-code/commit/76d5cf7d7f4d483def1bcee763752e0bff0be27a))
* **gate:** bound sandbox containers and make their image read-only ([#320](https://github.com/hoanghainh1188/aegis-code/issues/320)) ([#325](https://github.com/hoanghainh1188/aegis-code/issues/325)) ([c54a89f](https://github.com/hoanghainh1188/aegis-code/commit/c54a89f470a7e4b7146fe03658e467b1e0c19507))
* **gate:** read failing tests of go test, Surefire, Jest/Vitest and dotnet test ([#291](https://github.com/hoanghainh1188/aegis-code/issues/291)) ([#304](https://github.com/hoanghainh1188/aegis-code/issues/304)) ([7e721db](https://github.com/hoanghainh1188/aegis-code/commit/7e721db370750d7d17a2a1e5454bf8d8758d0ddd))
* **gate:** rerun suspected new test failures on their own, several times ([#306](https://github.com/hoanghainh1188/aegis-code/issues/306)) ([#308](https://github.com/hoanghainh1188/aegis-code/issues/308)) ([1b2a2fe](https://github.com/hoanghainh1188/aegis-code/commit/1b2a2fe56fb72fb85887c19ea05dc6b22d0a7832))
* keep the PR's patch and show it on the job and review pages ([#312](https://github.com/hoanghainh1188/aegis-code/issues/312)) ([#327](https://github.com/hoanghainh1188/aegis-code/issues/327)) ([593961b](https://github.com/hoanghainh1188/aegis-code/commit/593961b206fd71d4c2a1c6690f9fb6ce92ede68a))
* **server:** secrets read from files for on-premise deployments ([#261](https://github.com/hoanghainh1188/aegis-code/issues/261)) ([#275](https://github.com/hoanghainh1188/aegis-code/issues/275)) ([b7453c2](https://github.com/hoanghainh1188/aegis-code/commit/b7453c292a4935ab19ed6c2079f8dc00c8346229))
* show what a job works on and what the agent did ([#316](https://github.com/hoanghainh1188/aegis-code/issues/316)) ([#324](https://github.com/hoanghainh1188/aegis-code/issues/324)) ([a7d70ef](https://github.com/hoanghainh1188/aegis-code/commit/a7d70eff91304908f7071150e45069f9a61bc58c))
* **web:** admin sees and changes who has which role in each repo ([#329](https://github.com/hoanghainh1188/aegis-code/issues/329)) ([#332](https://github.com/hoanghainh1188/aegis-code/issues/332)) ([9222ce2](https://github.com/hoanghainh1188/aegis-code/commit/9222ce2940b66a8fc301a3929b6e1b67fd926e65))
* **web:** create, cancel and retry jobs from the pages ([#309](https://github.com/hoanghainh1188/aegis-code/issues/309)) ([#315](https://github.com/hoanghainh1188/aegis-code/issues/315)) ([870e670](https://github.com/hoanghainh1188/aegis-code/commit/870e670fb3908dd5e7b55e97676a76af64f54719))
* **web:** job page and job list update themselves while a job is live ([#310](https://github.com/hoanghainh1188/aegis-code/issues/310)) ([#319](https://github.com/hoanghainh1188/aegis-code/issues/319)) ([31100c8](https://github.com/hoanghainh1188/aegis-code/commit/31100c86a9ded8a75c9b3dee7ff4dee18b0bd06b))
* **web:** translate user interface to English ([#299](https://github.com/hoanghainh1188/aegis-code/issues/299)) ([870c129](https://github.com/hoanghainh1188/aegis-code/commit/870c12977cbb96fdd2e24c14bc9d7068cf177df3))


### Bug Fixes

* **aci:** an edit is not refused for syntax errors the file already had ([#289](https://github.com/hoanghainh1188/aegis-code/issues/289)) ([#292](https://github.com/hoanghainh1188/aegis-code/issues/292)) ([f28be55](https://github.com/hoanghainh1188/aegis-code/commit/f28be55ada5ff7300de4db1ab455347b3ce63d89))
* **eval:** the replay answers the code graph's impact note ([#280](https://github.com/hoanghainh1188/aegis-code/issues/280)) ([#287](https://github.com/hoanghainh1188/aegis-code/issues/287)) ([804864c](https://github.com/hoanghainh1188/aegis-code/commit/804864cec8ab1ef75cf6d0cdb4b83e2b351ab315))
* **gate:** fetch the base copy's in-tree dependencies for C# and TypeScript ([#305](https://github.com/hoanghainh1188/aegis-code/issues/305)) ([d5db4b1](https://github.com/hoanghainh1188/aegis-code/commit/d5db4b17e67c27baaf7de372e7c84e53da614895))
* **lang:** Java fetch step downloads the Surefire provider the gate uses ([#288](https://github.com/hoanghainh1188/aegis-code/issues/288)) ([#302](https://github.com/hoanghainh1188/aegis-code/issues/302)) ([b89816b](https://github.com/hoanghainh1188/aegis-code/commit/b89816b37bffc9c699106b31f41038f7bba3844f))
* **lang:** registry hosts for the Go, TypeScript, Java and C# fetch steps ([#279](https://github.com/hoanghainh1188/aegis-code/issues/279)) ([#285](https://github.com/hoanghainh1188/aegis-code/issues/285)) ([098f28d](https://github.com/hoanghainh1188/aegis-code/commit/098f28d29e386b75c239ab901d33a12f17d5ab80))
* **lang:** the C# profile caches MSBuild's artifacts output ([#290](https://github.com/hoanghainh1188/aegis-code/issues/290)) ([#293](https://github.com/hoanghainh1188/aegis-code/issues/293)) ([cbd8dd2](https://github.com/hoanghainh1188/aegis-code/commit/cbd8dd2f4d072496c378836027f2a9f1b3ecc3e3))
* **lifecycle:** aegis-aci reaches the Docker daemon the gate uses ([#278](https://github.com/hoanghainh1188/aegis-code/issues/278)) ([#286](https://github.com/hoanghainh1188/aegis-code/issues/286)) ([8127307](https://github.com/hoanghainh1188/aegis-code/commit/8127307232df352ef0532619fc70f5f681e755a0))


### Documentation

* gate sandbox on Kubernetes with a Docker-in-Docker sidecar ([#264](https://github.com/hoanghainh1188/aegis-code/issues/264)) ([#282](https://github.com/hoanghainh1188/aegis-code/issues/282)) ([be42815](https://github.com/hoanghainh1188/aegis-code/commit/be42815b026c14f64942829ca2d222896232a1ca))
* on-prem runbook fixes from a run of the local-image deploy ([#300](https://github.com/hoanghainh1188/aegis-code/issues/300)) ([#301](https://github.com/hoanghainh1188/aegis-code/issues/301)) ([4990f17](https://github.com/hoanghainh1188/aegis-code/commit/4990f175d4dc72ad308b4c1136e8eafdbf4e845d))
* onboarding for newcomers — release status, product picture, reviewer guide ([#297](https://github.com/hoanghainh1188/aegis-code/issues/297)) ([#298](https://github.com/hoanghainh1188/aegis-code/issues/298)) ([7bf4564](https://github.com/hoanghainh1188/aegis-code/commit/7bf4564adae74ce0a2c00db69ecbee72e1efba17))
* **readme:** illustrate external integrations and internal crate architecture ([#318](https://github.com/hoanghainh1188/aegis-code/issues/318)) ([c6e7011](https://github.com/hoanghainh1188/aegis-code/commit/c6e70110b414abfb208f2f023da84557f8ef7d3a))
* restructure README with workflow, multi-track quickstart and operations ([#284](https://github.com/hoanghainh1188/aegis-code/issues/284)) ([7793031](https://github.com/hoanghainh1188/aegis-code/commit/7793031b87c030ac7d7a72767bd3182c1685a424))
* translate operations runbooks, ADRs, and technical specs to English ([#296](https://github.com/hoanghainh1188/aegis-code/issues/296)) ([6ac952f](https://github.com/hoanghainh1188/aegis-code/commit/6ac952f257717afb0fad9b451f7c9684dc0cb0a9)), closes [#295](https://github.com/hoanghainh1188/aegis-code/issues/295)
