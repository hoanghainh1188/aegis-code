# aegis

The developer's client of [aegis-code](https://github.com/hoanghainh1188/aegis-code): send an issue to the platform and follow the job until its pull request. It only calls `aegis-server`'s API with your personal token.

```bash
npm install -g @hoanghainh/aegis
aegis login https://<server>
aegis submit <repo> <issue>
aegis watch <job>
```

Binaries for macOS (arm64, x86_64) and Linux (arm64, x86_64) are in the package; nothing is downloaded at install. Usage: [docs/client.md](https://github.com/hoanghainh1188/aegis-code/blob/main/docs/client.md).
