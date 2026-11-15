# CommonBot user guide

CommonBot lets you build agents on your own machine: define what each one handles, give it material to answer from, and let it search the web when needed. You can also talk to the Builder to define a new agent and publish it for others. All data stays on your machine.

## 1. Install and start

### Spark or another Linux machine

1. Unpack the package and enter the folder.
2. Run `./commonbot`, then open `http://localhost:8600` in a browser on that machine.
3. To start it automatically at boot: `sudo ./install.sh`. This installs the program to `/opt/commonbot` and registers a system service that runs as your current user. To remove it: `sudo /opt/commonbot/uninstall.sh`. Your data is not deleted.

If the machine has no display, turn on LAN access from a terminal, note the access code it prints, and use another device (see section 8):

```
./commonbot --lan on
```

(After installing the service with `install.sh`, use `/opt/commonbot/commonbot --lan on`, then `sudo systemctl restart commonbot`.)

### Mac

1. Unpack the package.
2. Before first use, run this once in Terminal on the unpacked folder (the program is unsigned, so macOS refuses to open it otherwise): `xattr -dr com.apple.quarantine path-to-folder`
3. Double-click “Start CommonBot.command” in the folder. Your browser opens automatically.

### Desktop app (optional)

Besides the archive above there is a desktop app with its own window: a `.deb` for Linux (`sudo apt install ./CommonBot_version_arm64.deb`) and a `.dmg` for Mac. It runs the service in the background and shows the interface. Closing the window leaves the service running; quit from the tray icon's menu. It keeps data in the same place as the archive version, so you can switch between the two, but don't run both at once.

## 2. Prepare models

CommonBot does not include models. It connects to an inference service on your machine. Ollama is the easiest: install it, pull a model, and CommonBot finds it by default (`http://127.0.0.1:11434/v1`). Any service with an OpenAI-compatible API (vLLM, llama.cpp and others) also works; add it on the Models page.

Three kinds of model are useful:

| Purpose | Notes | Without it |
| --- | --- | --- |
| Chat model | Required. Answers questions and powers the Builder | No chat |
| Embedding model | Lets the knowledge base search by meaning, e.g. bge-m3 | Keyword search only |
| Vision model | Reads text in scans and images | Scans and images can't be read |

### Compute pack: the inference service and models, installed offline

If you would rather not install Ollama and pull models yourself, use a compute pack: a folder named `compute` holding the inference runtime and a chosen set of models. When CommonBot starts and finds it, it starts the runtime itself and assigns the chat, embedding and vision models. No network and no further setup are needed.

Put the `compute` folder in either place:

- in the same folder as the `commonbot` program (on Linux, put it there before running `sudo ./install.sh`, which installs it to `/opt/commonbot` as well);
- in the data folder (see section 9; the desktop app uses this place).

The default set is about 35 GB, chosen for a machine with 48 GB of memory: qwen3:14b for everyday chat, qwen3:30b as a stronger alternative, bge-m3 for embedding and qwen2.5vl:7b for vision. The top of the Models page shows the pack's status. If Ollama is already running on the machine, CommonBot uses it and does not start a second copy. The runtime's log is `logs/compute.log` in the data folder.

The pack is built by whoever publishes it, on a machine with internet access (`scripts/compute-pack.sh` in the source), and can carry other models.

### Installing models without internet access

Pull the models with Ollama on a machine that is online, then copy its whole models directory to the same place on this machine:

- Linux: `/usr/share/ollama/.ollama/models` (when installed as a service) or `~/.ollama/models`
- Mac: `~/.ollama/models`

Restart Ollama, then click “Check now” on CommonBot's Models page to see them.

## 3. First-time setup

Open the Models page:

1. The inference services are listed at the top. Each should show OK and a model count. If one can't be reached, check that it is running and the address is right.
2. Under “Models for the knowledge base”, choose an embedding model and a text recognition model (skip them if you have none yet).
3. Web search uses the built-in search by default and needs no setup; click “Try a search” to confirm this machine's network returns results. You can switch to a self-hosted SearXNG or a keyed search API instead.
4. For a fallback when the local model is unavailable, use “Add inference service”, set the type to Cloud, and fill in the address, key and default model.

## 4. Agents

The Agents page lists every agent. Click one to chat. “Configure” at the top right changes its definition.

- **What it handles / doesn't handle**: the more specific, the better it stays on task. It says so plainly when a question is outside its job.
- **Knowledge scope**: the knowledge base folders it searches by default.
- **What answers are based on**: no web / knowledge first, web if needed / always search the web.
- **Model**: fix a model, or leave it to whatever is picked in the chat.
- **Opening line, example questions**: shown in a new chat; click an example to ask it.

Below the input box you can attach files (used in that chat only, not added to the knowledge base), change which folders are searched for this chat, switch web search on or off, and change the model. Small numbers in a reply are source references; click one to see the original text.

## 5. Create an agent by talking (Builder)

1. Open Builder and click Start.
2. Answer its questions: who it is for, what problem it solves, what material it relies on, what it must not do, tone and format. You can attach samples or existing scripts.
3. The draft on the right fills in as you talk. If something is wrong, tell the Builder, or click Edit — anything you edit by hand is never overwritten afterwards.
4. Click “Run the test”: the Builder writes a few questions, lets the drafted agent answer, and judges the answers. Adjust and run again until you are satisfied.
5. Click “Publish as an agent”. It appears on the Agents page with its own link and API key.

## 6. Knowledge base

Folders are on the left of the Knowledge page and can be nested. Create a knowledge base first, then add subfolders and upload documents.

- Supported: PDF, Word (docx), PowerPoint (pptx), Excel, text, Markdown, web pages and images. Old doc and ppt files and archives are not supported.
- Uploads are processed in the background. The table shows progress; failures state the reason and can be retried.
- Scans need a text recognition model chosen on the Models page. Documents that failed for lack of one are processed again automatically once you choose it.
- “Test search” shows which passages a question finds.
- “Ask within this folder only” asks a question against the current folder and its subfolders.

When nothing in the knowledge base supports an answer, the agent says so instead of making one up.

## 7. Publishing for others

Click Publish at the bottom of an agent's configuration (a model must be fixed first). You get:

- **A link**: anyone who opens it can chat with this agent. No access code is needed, and nothing else of yours is visible.
- **An API key and address**: an OpenAI-compatible API that existing clients and programs can use directly. Example:

```
curl http://host-address:8600/v1/chat/completions \
  -H "Authorization: Bearer your-key" \
  -H "Content-Type: application/json" \
  -d '{"messages":[{"role":"user","content":"Hello"}]}'
```

Unpublishing disables the link and key at once. “New key” disables only the old key. For other devices to use the link or API, LAN access must be on (next section).

## 8. Access from other devices

By default only this machine can use CommonBot. Turn on “Allow other devices” under System & appearance; the page then lists the addresses. Other computers and phones open one in a browser and enter the access code the first time.

The access code is shown and changed under System & appearance, only on the machine running CommonBot. After a change, other devices must enter the new code.

## 9. Where data lives and how to back it up

Everything is in one folder:

- Linux: `~/.local/share/CommonBot`
- Mac: `~/Library/Application Support/CommonBot`

`commonbot.db` holds agents, chats and knowledge base content; `files` holds the uploaded originals. To back up, quit CommonBot and copy the whole folder. To restore, copy it back. Use `--data-dir` at start-up to keep data elsewhere.

## 10. Upgrading

Download the new package, quit the old version, and replace the program files (on Linux, run `sudo ./install.sh` again). Leave the data folder alone; the new version upgrades the data automatically. Do not open data written by a newer version with an older program.

## 11. Troubleshooting

- **The model service can't be reached**: check that Ollama (or your service) is running, then click “Check now” on the Models page.
- **Replies are slow**: large models are slow to produce text. Use a smaller model for everyday questions.
- **PDF files can't be added; the PDF engine was not found**: the `lib` folder from the package must stay next to the program.
- **It won't open on a phone**: make sure “Allow other devices” is on, the phone is on the same network, and you are using an address listed on the page.
- **Forgot the access code**: open System & appearance on the host, or run `./commonbot --access-code` in a terminal.
