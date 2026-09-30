# Grok as a general-purpose agent (product surfaces, as of 2026-09-29)

Scope is consumer and product “Grok bot” surfaces: grok.com / the Grok iOS and Android apps, Grok on X, scheduled work, voice, computer or browser use, and the documented agent harness. The xAI API is included only to separate it from the apps. Grok Build (coding CLI: `/goal`, plan mode, sandbox, subagents) is a different product and is not inventoried here. Docs at docs.x.ai are titled “SpaceXAI (xAI)”; news pages are copyright SpaceXAI LLC. Help pages on help.x.com still attribute the model to xAI.

Source caveat: Grok Bot pages on docs.x.ai also say the computers run in Cursor’s cloud, that access can come from paid Cursor plans, and that account and privacy settings are Cursor’s. Those claims are reported as written; they were not independently checked against cursor.com.

## Which Grok surfaces are agents (multi-step tool use) versus chat?

### Takeaway
Official docs split three product surfaces: grok.com and the Grok mobile apps are a chat assistant that can also call connectors, skills, Imagine, voice, and scheduled automations; Grok inside X is a chat assistant that may search public posts and the web; Grok Bot is a separate multi-step agent with a persistent cloud computer. The API can run a server-side tool loop, but that is not the consumer app.

### Cited Findings
- Grok on grok.com and the iOS and Android apps is described as xAI’s assistant. Documented consumer actions are chat, creating images and video with Grok Imagine, hands-free voice, file upload, and connecting tools so Grok can reach email, files, and calendar inside a chat. Sign-in syncs conversations, settings, and subscription across those platforms. — [Welcome to Grok](https://docs.x.ai/grok/overview)
- The same FAQ says Grok Bot is not the same product as Grok on grok.com or the Grok mobile apps. Grok Bot is “durable AI teammates on a persistent cloud computer — messaging, approvals, connectors, and routines.” — [Grok website / apps FAQ](https://docs.x.ai/grok/faq)
- Grok Bot docs say you message a Bot (type, dictate, or voice chat), give it a task and tool access, and it does multi-step work across apps and websites, including drafts you approve before they are sent. Each Bot has a persistent cloud computer with a browser, filesystem, and terminal; it uses connectors where available and computer use otherwise, and work continues while the laptop is closed. Bots can run in parallel, message each other, share group chats, and pass task ownership. — [Grok Bot overview](https://docs.x.ai/grok-bot/overview)
- On X, Grok is an AI assistant for answering questions, solving problems, and brainstorming. When answering text or voice queries it can decide whether to search public X posts and do a real-time web search. It is reached from the Grok icon on x.com and the X iOS and Android apps. — [About Grok](https://help.x.com/en/using-x/about-grok)
- X Premium raises Grok usage limits. Premium+ includes SuperGrok, Grok Bot (“AI teammates that work in the background, signs into your apps, and actually get jobs done”), Imagine for video and images, and voice mode. — [About X Premium](https://help.x.com/en/using-x/x-premium)
- Consumer automations (grok.com and the Grok iOS/Android apps, not the API) run a job on a schedule or on a matching incoming email. Each run “opens a real conversation, does the work,” and stores the thread. Instructions can @-mention a connector so Grok uses it on every run. — [Automations in Grok](https://x.ai/news/grok-automations)
- The xAI API, separate from the apps, can take built-in tools and loop: analyze the query, call a tool or answer, execute server-side tools, and continue until it has enough information. Built-in tools listed: Web Search, X Search, Code Interpreter, Image Generation, Collections Search, plus developer-defined function calling. — [Tools overview](https://docs.x.ai/developers/tools/overview)

### Inferences
- grok.com chat plus connectors is tool-using, but the docs do not describe a user-visible computer, browser, or terminal on that surface. The documented multi-step computer-use agent is Grok Bot.
- Grok on X is documented as search-augmented chat (web and public posts), not as a computer-use or connector agent.
- Scheduled automations are agent-like jobs (a fresh conversation per run, connectors and skills attached) rather than a one-shot chat reply, but they live on grok.com / the Grok app, not inside Grok Bot’s computer.

### Gaps
- No opened page describes an in-chat “agent mode” toggle on grok.com distinct from ordinary chat, connectors, skills, and automations.
- Whether a single grok.com turn can chain web search, X search, and code execution the way the API does is not stated in the consumer FAQ or overview. Those tool names appear on the API tools page only.

## What tools are documented (web search, X search, code execution, image gen, file reading, email/calendar, computer use, browser)?

### Takeaway
Consumer grok.com documents chat, Imagine (image and video), voice, file upload and analysis, office-document skills, and OAuth connectors (mail, calendar, drive, and a wider catalog). Web and X search are documented for Grok on X and for the API, not as named tools in the grok.com overview. Computer use, a shared browser, a terminal, and `/workspace` files are Grok Bot. Python code execution in a sandbox is an API tool.

### Cited Findings
- grok.com / apps overview: chat; Grok Imagine images and video; voice; file upload (PDFs, images, spreadsheets, code, audio, and more) for analysis, extraction, and summarization; connectors for email, files, and calendar. — [Welcome to Grok](https://docs.x.ai/grok/overview)
- File upload in chat: web up to about 100 files, Android up to 20, iOS “multiple files.” Types include PDF, DOCX, TXT, CSV, XLSX, PPTX, HTML, XML, JSON, Markdown, LaTeX, ODT, RTF, some code extensions, JPEG/PNG/WebP/HEIC/BMP, MP3/WAV/M4A/OGG/FLAC/AAC, and MP4/MOV. Most files up to 150 MB. GIF and SVG support varies. Stated uses include summarizing, comparing, extracting tables, understanding images and charts, transcribing audio/video, and “debug or run code” on an attached file. Very long files may be summarized or handled in sections. Embedded images inside non-PDF files may not be processed visually. Delete assets at grok.com/files. — [Grok FAQ, Files & Data](https://docs.x.ai/grok/faq)
- Generated images and videos on the consumer product include a Grok watermark. There is no setting to remove it, and removing or obscuring the watermark is prohibited. 720p video falls back to 480p after a tier cap. — [Grok FAQ, Image & Video](https://docs.x.ai/grok/faq)
- Skills on grok.com, iOS, and Android (announced for Grok 4.3): built-in skills for Word (.docx), presentations, spreadsheets, and PDFs (create, merge, split, extract), plus a Skill Creator. Users can override a built-in skill; the user’s version takes priority. A skill can be created in conversation, from an uploaded file, or from scratch, and is described as remembered across conversations. — [Skills in web, iOS, and Android](https://x.ai/news/grok-skills)
- Built-in consumer connectors, each OAuth: Gmail and Google Calendar (separate connectors), Google Drive (Docs, Sheets, Slides), OneDrive, Outlook Mail and Calendar, Microsoft Teams, SharePoint, Salesforce (explore objects, query, create, and update). Once connected, “Grok can use the connector’s tools automatically whenever your questions relate to that service.” — [Connectors](https://docs.x.ai/grok/connectors)
- Catalog connectors named on that page: Box, Canva, Gamma, GitHub, Linear, Meltwater (needs a Meltwater subscription), Notion, S&P Global, Vercel. Full catalog is grok.com/connectors. Custom MCP: user supplies a public server URL; Grok discovers that server’s tools. — [Connectors](https://docs.x.ai/grok/connectors)
- May 6, 2026 launch post (product, grok.com / iOS / Android) lists SharePoint (search/read; create/edit/update if write is enabled), Outlook (search inbox, calendar, meetings; draft and send email; create invites), OneDrive, Google Workspace (Gmail, Drive, Docs, Sheets, Calendar read and write), Notion, GitHub, Linear, and “Bring Your Own MCP.” Web path: + button → Connectors. Mobile: Settings → Connectors. — [Connectors in web, iOS, and Android](https://x.ai/news/grok-connectors)
- Gmail connector capabilities: search with Gmail operators, read full messages including attachments, compose and manage drafts, send/reply/forward when write/send permissions are enabled, and labels/trash/delete. Calendar: search, view, free/busy, create/update/delete events when write is enabled, RSVP, list calendars. — [Gmail & Google Calendar](https://docs.x.ai/grok/connectors/gmail-google-calendar)
- Grok on X may search public X posts and the public web while answering. Voice input is transcribed or translated. — [About Grok](https://help.x.com/en/using-x/about-grok)
- Adding Grok to an X Chat, or using “Ask Grok” / “Edit with Grok” on a message or image, is documented. Ask Grok opens the selection in the Grok tab and the content is no longer encrypted once sent to Grok. Users can also chat with a Grok Companion from Chat. — [About Grok](https://help.x.com/en/using-x/about-grok); [About Chat](https://help.x.com/en/using-x/about-chat) was only partially confirmed via the About Grok cross-link; Companion platform limits are in the FAQ below.
- Grok Bot computer: shared browser cookies and signed-in sessions, shared files, shared command-line credentials, per-Bot screen, one computer-use task per Bot at a time. **Agent Computer** previews clicks, typing, navigation, and status. Connectors are plugins from Marketplace; in chat, `@` attaches a connector and `/` references a skill. Docs say prefer a connector over clicking a site. Shared workspace path is `/workspace`. — [Use the computer and apps](https://docs.x.ai/grok-bot/computer-and-apps)
- A site can block automation, expire a session, or require a human step; the Bot is supposed to hand those steps to the user rather than work around them. — [Grok Bot overview](https://docs.x.ai/grok-bot/overview)
- API-only tool names and behavior (not stated as grok.com buttons): `web_search` (search and browse pages), `x_search`, `code_interpreter` / `code_execution` (Python in a sandbox), `image_generation` (and it can be combined with web search in one agentic loop), collections/file search. Server-side tools run on xAI servers; function calls return to the developer. — [Tools overview](https://docs.x.ai/developers/tools/overview)
- Docs index also lists separate developer voice APIs (speech-to-speech, text-to-speech, speech-to-text, custom voices) and files/collections. Those pages were not opened; only the index was. — [docs.x.ai llms.txt](https://docs.x.ai/llms.txt)

### Inferences
- “Run code” in the consumer file FAQ is an analysis claim, not documentation of the API code interpreter inside grok.com.
- Browser and desktop control are documented for Grok Bot’s cloud computer, not for the grok.com chat composer.

### Gaps
- No opened consumer page lists a grok.com tool named web search, X search, or code execution, even though Grok on X is documented to search posts and the web.
- Google Drive’s exact write operations were not opened (only the connectors index row and the May 2026 announcement).
- The voice-agent API post and voice API reference pages were not opened, so tool use inside developer voice sessions is not inventoried here.
- Companion behavior beyond “iOS only” and the X Chat mention was not opened.

## Does Grok have memory, projects, tasks/schedules, or connectors to outside accounts?

### Takeaway
Yes for connectors and for two different schedulers: grok.com Automations, and Grok Bot routines. Persistent “skills” are documented on grok.com. Editable cross-chat memory and Projects are not specified on any page that was opened, except a FAQ aside that Projects can fail to appear on the wrong host. Grok Bot documents per-Bot memory. Grok on X documents personalization from X data, which is not the same as a memory store.

### Cited Findings
- Connectors are available to all Grok users and authenticate with OAuth. On Grok Business and Enterprise, a team admin must provision a connector in the cloud console before members can use it. Custom MCP servers must be reachable on the public internet. — [Connectors](https://docs.x.ai/grok/connectors)
- localhost and private addresses (`127.0.0.1`, `10.x`, `172.16.x`, `192.168.x`) are rejected for custom MCP URLs. A tunnel is required. Cloudflare quick tunnels do not support MCP SSE; Streamable HTTP is said to work. — [Custom MCP Server Tunneling](https://docs.x.ai/grok/connectors/custom-mcp-tunneling)
- Automations on grok.com and the Grok iOS/Android apps: user writes instructions once, can attach files, connectors, and skills, and picks a mode. Schedules: once, daily, weekdays, weekly, monthly, or yearly, at a chosen local time. Email triggers match sender, recipient, or subject and pass that email in as context. User can Run now. Each run is a saved conversation the user can continue. Notifications: email, app, both, or neither. Create from chat or from templates at grok.com/automations. Pause, resume, edit, or delete. Scheduled automations are for everyone; email triggers are SuperGrok. — [Automations in Grok](https://x.ai/news/grok-automations)
- Skills on grok.com are “persistent expertise”: preferences, formatting rules, and workflow steps the user should not have to repeat. Built-ins ship with every account. — [Skills in web, iOS, and Android](https://x.ai/news/grok-skills)
- Usage accounting treats Chat, Imagine, Voice, and Build as separate products that draw one weekly pool for paid users (rolling out June 2026). Settings → Usage breaks down API, Build, Chat, Imagine, and Voice. After the weekly cap, paid features pause; free-tier Chat and Voice remain. Extra Usage Credits can be bought on the web only (from $5); mobile in-app purchase is “in the future.” — [Grok FAQ, Usage](https://docs.x.ai/grok/faq)
- FAQ: “Some users on grok.x.ai or other hosts run into missing features like Projects.” The correct web address given is grok.com. No Projects spec (what a project stores, who it is shared with) was on that page. — [Grok FAQ, Products](https://docs.x.ai/grok/faq)
- Grok on X personalization: X may share public profile, public posts, top posts, engagement, interests, and Grok inputs/results with xAI. Voice inputs and transcripts may be shared. Users can turn off “Allow X to personalize your experience with Grok” and separately opt out of training. Private accounts’ posts are not used to train or to be surfaced in replies to other users’ queries. Conversation history can be deleted and is removed within 30 days unless kept for security or legal reasons. — [About Grok](https://help.x.com/en/using-x/about-grok)
- Grok Bot memory, per the FAQ on its overview: “Stable preferences, role context, and summaries of prior work.” Conversations and learned context stay separate per Bot. Shared files, browser sessions, and direct handoffs move context between Bots. Docs say not to rely on memory for consequential decisions. — [Grok Bot overview](https://docs.x.ai/grok-bot/overview)
- Grok Bot skills are reusable instructions (when to use, inputs, sequence, validation, output, what needs approval), shared as one private library across the user’s Bots. **Teach a task** records a browser workflow for up to ten minutes (no microphone audio) into a draft skill; rollout may be gradual. — [Skills and routines](https://docs.x.ai/grok-bot/skills-routines-and-automations)
- A Grok Bot routine is a schedule or, where supported, an event trigger owned by one Bot. Event examples named: a Slack message or a GitHub notification via “Cursor account integrations,” described as separate from Slack/GitHub plugins. Routines keep running with the laptop closed. A Bot can own up to 50 routines; the app keeps the 20 most recent run records per routine. After a long absence Grok Bot may ask whether to keep routines running and pause them if there is no response. Time zone for routines is a Bot setting. — [Skills and routines](https://docs.x.ai/grok-bot/skills-routines-and-automations)
- All of a user’s Bots share one cloud computer (files, browser logins, CLI credentials). Isolation between users is described as strict. The computer is not the user’s Mac or Windows machine. — [Use the computer and apps](https://docs.x.ai/grok-bot/computer-and-apps)

### Inferences
- “Memory” on grok.com, in the pages opened, means saved skills plus normal chat history, not a documented user-editable memory file. Personalization memory is an X data-sharing setting.
- Projects exist as a named grok.com feature at least enough for support to say the wrong host drops them. What they contain is undocumented here.
- grok.com Automations and Grok Bot routines are different schedulers on different products. Email-triggered automations are a SuperGrok grok.com feature; Grok Bot event triggers are described via Cursor account integrations.

### Gaps
- No opened page documents a consumer memory UI (view, edit, or delete individual memories) for grok.com or the Grok app.
- No opened page specifies Project contents, sharing, or instructions files.
- Team Bots (Sept 28, 2026 news) were not opened; shared team memory and plugins are not cited.
- Grok Build’s project memory (`/memory`, `/dream`) was not treated as a general-agent feature and was not opened beyond search snippets.
- Whether grok.com automations can send email or change calendar events without an extra confirm step is not stated in the automations post.

## What permission or confirmation model exists before side effects?

### Takeaway
On grok.com, permission is mostly OAuth scope and, for Business/Enterprise, an admin allow-list. A per-action confirm dialog before send/delete is not documented for grok.com chat or automations. Grok Bot documents explicit approval prompts (Allow once / Always allow / Deny), Auto Review rules, human takeover for secrets and payments, and a separate local-computer execution switch that defaults to ask every time.

### Cited Findings
- Consumer connectors: user completes OAuth; “Grok will request only the permissions it needs.” After that, connector tools can be used automatically when the question relates to the service. Business/Enterprise admins provision connectors before members can connect them. — [Connectors](https://docs.x.ai/grok/connectors)
- Gmail is tiered: base connection is read-only (`gmail.readonly`). `gmail.modify`, `gmail.send`, and `gmail.labels` are requested only when those tools are enabled. The Gmail page says write and send “are enabled progressively by your organization’s administrators.” Calendar write (`calendar.events`) is likewise only when write tools are enabled. Disconnect is immediate from grok.com/connectors or Google’s permissions page. The page says SpaceXAI does not train on Gmail/Calendar data and does not store that data from connector conversations; access is real time. — [Gmail & Google Calendar](https://docs.x.ai/grok/connectors/gmail-google-calendar)
- Automations: the user authors instructions, attaches connectors with `@`, chooses schedule or email filter, and chooses how to be notified. The post does not describe a confirm step inside a run. Email triggers are limited to SuperGrok. — [Automations in Grok](https://x.ai/news/grok-automations)
- Grok on X: training and personalization are separate opt-out toggles under Privacy & Safety → Data sharing and personalization → Grok & Third-party Collaborators. Opting out of training does not stop a deployed model from learning during normal use of X features powered by Grok. Thumbs-up/down feedback can still be used for training. Help text says this is an early version that may be factually wrong and tells users not to share personal or sensitive data. X says it does not sell user data. — [About Grok](https://help.x.com/en/using-x/about-grok)
- If Grok is added to an X Chat, that conversation is no longer end-to-end encrypted. Removing Grok restores E2EE for later messages only; messages sent while Grok was present stay unencrypted. — [About Grok](https://help.x.com/en/using-x/about-grok)
- Grok Bot: the user can put a boundary in the request (do not send, publish, purchase, delete, change permissions, touch production, or accept legal terms). When an action needs approval, the conversation shows the proposed operation and inputs. **Allow once** continues that action, **Always allow** can save a matching rule, **Deny** blocks it. The same controls are on iPhone. An approval does not undo work already done. — [Approvals, security, and privacy](https://docs.x.ai/grok-bot/approvals-security-and-privacy)
- Auto Review (Settings → General → Auto-review) evaluates tool calls and computer actions. **Ask first** always stops a match. **Allow automatically** proceeds only if automated review finds no other reason to stop. If both match, Ask first wins. Team-enforced rules are locked; personal rules can only make behavior stricter. Personal rules are stored on the current desktop and synced to that desktop’s Grok Bot computer. Docs say Auto Review is model-based and should not replace least privilege, and warn against rules like “allow everything in the browser.” — [Approvals, security, and privacy](https://docs.x.ai/grok-bot/approvals-security-and-privacy)
- Secrets: passwords, passkeys, 2FA, CAPTCHAs, and payments should be a user takeover of Agent Computer, not text in chat. A “secure secret request” for a supported connection is masked, kept out of the transcript, and not shown to the model. A chat form can collect a login, checkout address, or phone number and fill the page. Hardware security keys (for example YubiKey) work from the desktop while the setting is on (default on macOS and Windows, not supported on Linux); every use asks for approval. — [Approvals, security, and privacy](https://docs.x.ai/grok-bot/approvals-security-and-privacy); [Use the computer and apps](https://docs.x.ai/grok-bot/computer-and-apps)
- Local computer: cloud computer is separate. Settings → General → Bot → Execution on Local Computer: **Ask every time** (default), **Always allow**, or **Never allow**. After computers are registered, the control moves per computer. First prompt: allow Grok Bot and all Bots to run commands locally, with Always allow, Allow once, Never, and Deny once. A stricter team ceiling overrides the user. These settings do not block the cloud computer. — [Approvals, security, and privacy](https://docs.x.ai/grok-bot/approvals-security-and-privacy)
- Grok Bot says it uses Cursor authentication and account data settings, requires data storage, does not support Legacy Privacy Mode, and that training opt-out follows Cursor account settings. A public Bot share link copies configuration, not the computer or logins. Deleting a Bot does not delete shared-computer files or browser sessions. — [Approvals, security, and privacy](https://docs.x.ai/grok-bot/approvals-security-and-privacy)
- A Grok Bot test run “performs real work”: it can navigate websites, change files, and call tools. Docs tell users to keep write actions behind approval. Routines should draft first and require approval before sending, purchasing, deleting, publishing, or changing production. — [Skills and routines](https://docs.x.ai/grok-bot/skills-routines-and-automations)
- Sharing one computer is explicitly not a security boundary between that user’s Bots. Connector installs are account-wide. — [Use the computer and apps](https://docs.x.ai/grok-bot/computer-and-apps)

### Inferences
- For consumer grok.com, the documented gate before mail/calendar side effects is whether write/send scopes were granted (and, for orgs, whether an admin enabled them), not a per-message human prompt.
- Grok Bot’s confirmation model is the one that matches an agent harness: proposed action, once vs always, deny rules that outrank allows, and a default prompt before local shell execution.

### Gaps
- No opened grok.com page says the chat UI asks “send this email?” after Gmail send scope is on.
- Auto Review’s exact action list (shell, plugins, computer use, and so on) is on the security FAQ, which was not opened; only the approvals page was.
- What “when write tools are enabled” means for a personal SuperGrok account, as opposed to an organization admin, is not spelled out beyond the OAuth tables.

## What do official docs say is not available?

### Takeaway
Docs explicitly retire Grok Studio, keep Companions off web and Android, refuse private MCP URLs, refuse watermark removal, and say Grok Bot is not the grok.com app. They also limit free access after the weekly cap to Chat and Voice, limit Extra Usage purchases to the web, and warn that Grok on X can be wrong and must not receive sensitive data. Browser automation is not guaranteed to pass logins, CAPTCHAs, or anti-bot checks.

### Cited Findings
- **Grok Studio is no longer supported.** The FAQ says to use Grok Build instead, and to revoke credentials if a third-party “Studio” app is using the Grok account. — [Grok FAQ](https://docs.x.ai/grok/faq)
- **Companions are iOS only.** “No — Companions are available on the iOS app only, and there are no plans to bring them to the web or Android.” — [Grok FAQ](https://docs.x.ai/grok/faq)
- grok.x.ai and other hosts can miss features such as Projects. The stated web app is grok.com. — [Grok FAQ](https://docs.x.ai/grok/faq)
- No consumer control removes the Grok watermark on generated images and videos. Altering provenance signals is prohibited. — [Grok FAQ](https://docs.x.ai/grok/faq)
- Extra Usage Credits cannot currently be purchased in the mobile apps. — [Grok FAQ](https://docs.x.ai/grok/faq)
- When the paid weekly pool is exhausted, paid features pause. Remaining access called out: free-tier Chat and Voice only. — [Grok FAQ](https://docs.x.ai/grok/faq)
- API credits are non-refundable (API billing, not the consumer app). — [Grok FAQ](https://docs.x.ai/grok/faq)
- Custom MCP on localhost or RFC1918-style private IPs is rejected. — [Custom MCP Server Tunneling](https://docs.x.ai/grok/connectors/custom-mcp-tunneling)
- Grok Bot cannot treat a website as always automatable: blocks, expired sessions, CAPTCHA, 2FA, payment, or an explicit human check are handed back to the user. Docs say not to bypass those checks. — [Grok Bot overview](https://docs.x.ai/grok-bot/overview); [Use the computer and apps](https://docs.x.ai/grok-bot/computer-and-apps)
- One Bot cannot run two computer-use tasks on its screen at once. Separate Bot screens are not separate security boundaries. Temporary directories, manually installed packages, and uncommitted app state on the cloud computer are replaceable. Reset rebuilds from the last snapshot and can lose very recent changes. — [Use the computer and apps](https://docs.x.ai/grok-bot/computer-and-apps)
- Linux desktop: hardware security keys for the Bot browser are not supported. — [Approvals, security, and privacy](https://docs.x.ai/grok-bot/approvals-security-and-privacy)
- Teach-a-task recording does not include microphone audio, caps at ten minutes, and may be absent while rollout is gradual. — [Skills and routines](https://docs.x.ai/grok-bot/skills-routines-and-automations)
- Grok Bot does not support Legacy Privacy Mode and requires data storage. — [Approvals, security, and privacy](https://docs.x.ai/grok-bot/approvals-security-and-privacy)
- Grok on X: “early version”; may confidently state incorrect facts, missummarize, or miss context. Users are told not to share personal or confidential information. Private posts are excluded from training and from being surfaced to other users’ queries. Opting out of training does not stop learning from normal use of Grok-powered X features. — [About Grok](https://help.x.com/en/using-x/about-grok)
- X Chat is not end-to-end encrypted while Grok is a participant. — [About Grok](https://help.x.com/en/using-x/about-grok)
- xAI says it provides Grok in X on X.com and the X apps but does not have operational oversight of X’s service. — [Grok FAQ](https://docs.x.ai/grok/faq)
- File limits: embedded images in non-PDF documents may not be seen; audio/video transcription quality is variable; GIF/SVG support is inconsistent. — [Grok FAQ](https://docs.x.ai/grok/faq)
- Email-triggered automations are not on the free tier; they require SuperGrok. Scheduled automations are available to everyone. — [Automations in Grok](https://x.ai/news/grok-automations)

### Inferences
- Docs do not claim a general-purpose OS sandbox or confirmation gate for grok.com chat. The “not available” list is mostly product retirements, platform limits, and Grok Bot’s refusal to skip human checks.
- A user who only has Grok inside X does not, on these pages, get connectors, automations, computer use, or Grok Bot unless they also have the matching subscription (Premium+ text bundles SuperGrok and Grok Bot).

### Gaps
- Acceptable Use Policy text was not opened, so banned content categories are not listed.
- No opened page says grok.com lacks a browser tool in those words; the overview simply does not list one, while Grok Bot docs do.
- Live grok.com and grok.com/automations UI were not opened (marketing and docs pages were). If the app has moved ahead of the docs, that drift is unmeasured.
- help.x.com/en/using-x/about-chat was not fully fetched; Companion and “Ask Grok” details beyond the About Grok summary are thin.
