# Changelog

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
