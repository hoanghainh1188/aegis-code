# Self-Hosted presets (#599)

Each directory holds a mode file for one kind of deployment. The models, the residency, and the embedding model used for the memory's semantic recall are already chosen. CI loads every preset as the server would: `crates/aegis-lifecycle/tests/presets.rs` checks it against its own `[residency]` (and, for the Japan preset, the customer-jp guard) and checks that every model has a price.

| Preset | Models | Code and memory stay in | Secret |
|---|---|---|---|
| `aws-japan-bedrock` | Bedrock `jp.` profiles, Titan embeddings v2 | `ap-northeast-1` | none: the server's IAM role |
| `gcp-vertex-tokyo` | Claude on Vertex AI, `text-multilingual-embedding-002` | `asia-northeast1` | `VERTEX_ACCESS_TOKEN` |
| `azure-openai-enterprise` | your Azure OpenAI deployments | Japan East | `AZURE_OPENAI_API_KEY` |
| `onprem-airgap-ollama` | Ollama: `qwen2.5-coder`, `nomic-embed-text` | the deployment's network | none |

## Use one (deploy/compose)

1. Copy the mode file. For Azure, also copy its pricing file and put your contract's prices in it.

   ```bash
   cp deploy/presets/gcp-vertex-tokyo/mode.toml deploy/compose/modes/gcp-vertex-tokyo.toml
   ```

2. Name it in `server.toml`:

   ```toml
   [modes]
   gcp-vertex-tokyo = "/etc/aegis/modes/gcp-vertex-tokyo.toml"
   ```

   Name `aws-japan-bedrock` as `customer-jp`, so the guard of design §7 applies.

3. Put the preset's secret in `./secrets` (one file per secret, named after it).
4. Edit what is yours:
   - Vertex: the project;
   - Azure: the resource URL, the deployment names and the prices;
   - Ollama: the host, if it is not the `ollama` service.
5. Semantic recall needs PostgreSQL with pgvector. Set `AEGIS_POSTGRES_IMAGE=pgvector/pgvector:pg17` (see `docker-compose.yml`), or drop `[memory.embedding]`. Without pgvector, recall stays lexical.
6. `./aegis-deploy.sh <version>`. A mode that breaks its residency stops the start and says which line is wrong.

Then add each repo from the dashboard (Admin, Repos, Add). Its page runs the pre-flight check and turns the repo on once the check passes.
