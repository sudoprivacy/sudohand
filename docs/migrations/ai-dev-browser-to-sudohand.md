# ai-dev-browser → sudohand 迁移规划

更新：2026-10-02 · 状态：sudowork 可选 Rust 后端已完成本地真实模型／UI 验收及 Python 回退验收；完整迁移尚未完成。

[ShareOne 阅读版](https://s.shareone.vip/md/ai-dev-browser-to-sudohand) · [规划 PR #28](https://github.com/sudoprivacy/sudohand/pull/28)

## 1. 决策与范围

**以 sudohand 作为后续工具层的主仓库；ai-dev-browser（下文 adb）在迁移期继续维护，所有归档门槛通过后再归档。** 现在不能把依赖里的包名替换完就宣布迁移成功。

迁移规划、依赖清单和固定版本源码参考已经建立。首批 Rust 行为修复及持续 audit 的本地验收见下文；调用方升级、正式发行和 adb 归档仍需通过后续门槛。本规划不触发新版本发布。

迁移保留三种调用需求：

- CLI：`python -m ai_dev_browser.tools.<name>` / `aidb` 的调用方逐步改用 `suh browser <name>`，同时处理参数、JSON、退出码和产物协议。
- Python SDK：直接持有 `CDP`、`Tab`、连接、事件或浏览器池对象的调用方，需要迁移实现，或使用有明确范围的兼容层。
- 集成与分发：桌面安装包、插件发现、技能模板、CI、示例、版本锁定和传递依赖分别登记、验收。

## 2. 基线与已知差距

| 对象 | 固定基线 | 用途 |
|---|---|---|
| adb | [v0.51.1 / c349d347](https://github.com/sudoprivacy/ai-dev-browser/tree/c349d347251779357136685b6a698e3d2c59ce6d) | 当前 61 个公开工具及其行为参考 |
| sudohand main | [c62b244a](https://github.com/sudoprivacy/sudohand/tree/c62b244a43d53bb87e1f14e97c5858ce41480745) | 本规划的代码起点 |
| sudohand parity 分支 | [PR #27 / 045202ed](https://github.com/sudoprivacy/sudohand/pull/27) | 已实际编译、用真实 Chrome 检查的 Rust 实现 |
| PR #27 原参考 | adb [v0.38.1 / 94170d23](https://github.com/sudoprivacy/ai-dev-browser/tree/94170d23f65f5f9140792d1de4a158c086a39325) | 解释为何原 parity CI 通过仍不等于当前版本验收 |

同名工具静态比对为 main 54/61、PR #27 59/61。**这只是名称覆盖，不是行为完成率。** PR #27 的 Linux/macOS/Windows [CI 记录](https://github.com/sudoprivacy/sudohand/actions/runs/34790916242)也不能替代当前基线的真实验收。

已对 PR #27 和 adb v0.51.1 做过 Windows 本地真实 Chrome/CDP 对照，共 39 次 CLI 调用，包含本地页面和 `example.com`。表单发现、填写、提交及截图通过；下列差异已观察到：

| 项目 | 实际观察 | 必须补的验收 |
|---|---|---|
| 拖拽 | Rust 返回成功，但移动事件 `buttons=0`，滑块位移 0；Python 位移 220px，`buttons=1` | 断言可信事件、按键状态和最终位置 |
| 找不到点击目标 | Rust `clicked=false` 却 exit 0；Python exit 4，并提供恢复提示 | CLI、flow、serve 都不能把失败当作完成 |
| 跨命令视口状态 | Rust 设置 390×844 后，下一次调用看到 1600×950；Python 保持 390×844 | 独立 CLI 进程之间读取真实视口 |
| 确定性 JS 异常 | Rust 返回 exit 9 / `io`；Python 标为不可重试 | 区分脚本错误和临时连接故障，避免盲目重试 |
| 新工具与参数 | 缺少录制 start/stop；PDF 纸型／带单位尺寸、iframe 下载参数不接受 | 完成录制、PDF、跨域下载实际文件验收 |

静态参数比对另发现 `mouse_click` / `mouse_move` 的 `--human-like`，以及 PDF `--margin`、`--prefer-css-page-size` 等差异。导航超时、扩展传输、OOPIF、Electron/CEF、桌面和 VLM 的完整行为尚未在本轮验收，必须在后续门槛中补齐。摘要记录见 [live-review.json](https://github.com/sudoprivacy/sudohand/blob/main/docs/migrations/live-review.json)。

### 2.1 首批运行时验收与持续 audit（2026-10-02）

在 PR #27 的实现上接入迁移基线，并修复可信拖拽的 `buttons`、跨进程窄屏视口、HTML id／XPath 点击目标缺失的退出码、确定性 JS 异常分类及启动失败的退出码。旧观察保留为历史记录；新证据见 [browser-foundation-acceptance.json](https://github.com/sudoprivacy/sudohand/blob/main/docs/migrations/browser-foundation-acceptance.json)。

- Windows 本地 live PTY、真实 Chrome：31 次 CLI 调用，验证 7 条流程；另有 27 个 Chrome SDK 集成测试通过。
- 真实 API key、Claude Opus 4.8：4 个工具选择／恢复场景、9 次模型请求。已知 id、失效定位器恢复、移动视口和拖拽均完成真实页面操作，并校验最终状态。该结果仅覆盖这些场景。
- 模型曾把 `--html-id` 猜成 `--id`。新增 `suh describe --domain browser --with-args`，直接从 Clap 参数定义生成目录；最终实跑使用了正确参数。六个工具的首段说明共用于 SDK 文档、CLI help 和目录。
- [CLI Steering Engineering](https://github.com/sudoprivacy/sudohand/blob/main/docs/cli-steering-audit.md) 已固定来源与 commit，写入根 `AGENTS.md`；支持复用现有 checkout 或一条命令下载验证。上游目前私有，因此采用可选 reference，公共 CI 不需要私有仓库权限。
- CI 自动运行命令契约和真实 Chrome 流程；提供付费模型测试的手动入口。当前仓库尚未配置 `ANTHROPIC_API_KEY` secret，模型验收已用本机现有配置完成，不能把普通 CI 通过称为模型验收通过。

首个调用方试验见 2.2 节。录制、PDF、SDK 深层依赖、源码许可追溯和发行／归档门槛继续保留。

### 2.2 sudowork 可选后端与真实消费者验收（2026-10-02）

[sudowork PR #1181](https://github.com/sudoprivacy/sudowork/pull/1181) 保留 `browser` 入口和默认 Python 后端，通过 `SUDOWORK_BROWSER_BACKEND=sudohand` 选择 Rust。测试固定 sudohand `ec9ae623`；旧后端使用 sudowork 当前固定的 adb `ec3b2151`，没有顺便升级 vendor。配置与回退步骤见 [接入说明](https://github.com/sudoprivacy/sudowork/blob/cabd0718d79c2c936de61056a658a1a39289f5c5/docs/tech/browser-backends.md)，机器记录见 [验收证据](https://github.com/sudoprivacy/sudowork/blob/cabd0718d79c2c936de61056a658a1a39289f5c5/docs/tech/browser-backend-acceptance.json)。

- 包装器从所选后端生成目录，保留 JSON、退出码、产物路径与 sidechannel 关联。失败时不自动切换后端或重放动作。
- 本地 live PTY、真实 Chrome、两后端共 31 次包装器调用：中文填写、可信提交且只提交一次、截图、刷新后收据、逐次 HTTP 结果关联、定位失败恢复及配置／参数错误通过。已加入 Windows/Linux PR CI。
- 真实 Electron 应用、scode 0.2.21、Claude Opus 4.8、真实 API 凭据：Rust 使用 7 次成功 PowerShell 调用，完成预订并读对只存在于图像像素中的随机徽章；点击会话附件后实际加载 1600×950 截图。移除后端变量并重启应用，Python 使用 11 次成功 PowerShell 调用完成相同任务。两次最终验收均无失败工具调用。
- 实测修复了 ACP 新配置未安装包装器、monorepo 开发路径找不到 Python 包、skill junction 无法被模型文件工具读取的问题。Windows skill 摘要和正文明确描述如何发现并调用独立 PowerShell 工具；修改摘要后须重启以清除索引缓存。
- UI 测试等待附件生成并请求实际绘制帧，避免被遮挡的 Electron 虚拟列表尚未重绘时误点同名工作区条目。通过记录保留真实预览截图的哈希；早期失败没有计入通过结果。
- 共享 skill 补充无 `ToolSearch` 宿主的原生 Windows shell 路径后，再跑 scode 真实模型／UI：9 次 PowerShell 调用、零失败工具、预订／读图／附件预览全部通过。条件式开头曾导致两次 Bash 失败，该措辞已撤回，失败保留在证据中；其他 ACP 宿主仍待单独验收。
- [CLI steering 上游改进](https://github.com/sudoprivacy/cli-steering-engineering/pull/1) 已合入，并将本仓库引用更新至 `5f149174`。后续 audit 覆盖 skill 摘要、实际文件路径、延迟加载工具和第一条执行调用。

本地 typecheck、应用构建和相关测试通过；全量 Windows 测试为 2799 通过、15 失败、31 跳过，15 项失败均在未修改基线上复现，未宣称全量通过。

**本轮只接受开发构建中的可选接入和回退。** 安装包分发、默认切换、其他消费者与深层 Python SDK 仍未验收。M1 下一项是 Grok pool 生命周期试验和 SDK 路线决策；同时继续补录制、PDF、下载／扩展行为，最终按 M3 验收正式发行。

### 2.3 Grok pool 的 Python 基线与持久化修复（2026-10-02）

Grok 的首次真实试验发现旧 adb 的 checkpoint 缺陷：读取成功后，Pydantic 结果里的 `datetime` 保持 Python 类型，导致 `save_state()`／关闭 pool 时 JSON 序列化失败。[adb PR #9](https://github.com/sudoprivacy/ai-dev-browser/pull/9) 改为在结果入口使用 Pydantic JSON 模式，并补上回归测试。源码参考仍固定 v0.51.1；此修复单独记录，不覆盖历史基线。

- 使用 Grok `44033c2d`、真实本地凭据与 Chrome，通过 live PTY 启动两个独立 worker，分别只读收藏列表，确认结果包含时间字段。保存两个完成任务及一个待办任务，关闭后用同一组 profile 和 checkpoint 重开；只执行待办任务，保留两个完成结果，最终三个任务完成且浏览器端口关闭。
- 最终脚本运行通过，耗时 22.7 秒。带诊断的前一轮也通过；另一次在重启连接 Chrome 时失败，尚未定位原因，保留为生命周期稳定性待查项。没有加入重试来掩盖失败，也没有换 profile 绕过重启路径。
- 本地单元和新增回归测试共 126 项通过；新增回归进入 Windows/macOS/Linux CI。真实账号脚本已提交，明确要求有效登录及带时间字段的收藏；缺少条件会失败。测试清除临时凭据、账号结果和专用 profile，仅保留脱敏记录。
- [脚本及验收证据](https://github.com/sudoprivacy/ai-dev-browser/blob/2bcfe9f661b5eaa0afc96122cb8182b01f9796c7/tests/integration/pool-persistence-acceptance.json) 区分真实浏览器验收与使用内存客户端的 CI 回归。CLI steering 检查对应规则 5：pool 返回值应在源头满足 JSON 序列化合同。
- Windows parity 另暴露 pool 等待超时、旧 Python 参考下载未落盘和扩展加载超时。本机完整 Rust 浏览器套件 101 项通过，原页面／下载对照流程也通过；这些结果没有用于抹去 CI 失败。修正扩展测试改写 Windows 账号环境的问题，补上可避开现有个人 bridge 的真实启动检查、正确的二进制 fixture MIME 和 pool 失败诊断；超时与文件内容断言保持原样。[诊断记录](https://github.com/sudoprivacy/sudohand/blob/main/docs/migrations/windows-parity-followup.json) 保留已确认的环境问题与尚未定位的失败。

**这里只接受 Python 后端的持久化修复。** Grok 的 Rust SDK 路线、取消／失败恢复、下载、扩展传输和下游固定版本仍未验收；M1 不能据此退出。下一步用同一条真实任务链比较消费者改写与有限 Python bridge，并追查重启连接失败。

## 3. 依赖盘点结果

扫描了本地 **142 个 Git 工作目录**、远端 **123 个仓库的默认分支**（120 个非空），远端共检查 60,791 个选定文本文件。远端 28 个仓库直接出现 adb 引用，另有 2 个仅通过已知上游形成传递依赖；加上本地发现的一个历史文档仓库，去重后共 31 个相关仓库：

| 分类 | 数量 | 是否需要迁移验收 |
|---|---:|---|
| 运行、测试、技能、示例或分发方 | 23 | 是；先确认维护状态和旧后端可运行性 |
| 仅文档引用 | 6 | 更新当前推荐；历史证据可保留 |
| adb 源仓库与 sudohand 目标仓库 | 2 | 作为基线／交付方单独跟踪 |

23 个调用／分发方中，5 个公开仓库、18 个受限仓库。公开证据及匿名任务编号见 [dependency-inventory.json](https://github.com/sudoprivacy/sudohand/blob/main/docs/migrations/dependency-inventory.json)。受限仓库的名称、路径、版本和证据保存在本地完整台账中；在进入批量迁移前，须转存到团队可访问的私有跟踪位置，并保持 R 编号稳定。

### 3.1 公开调用方

| ID / 仓库 | 依赖性质与证据 | 迁移要求 |
|---|---|---|
| P05 · [sudowork](https://github.com/sudoprivacy/sudowork) | `.gitmodules`、`apps/desktop/electron-builder.yml` 打包 Python；`browser_helper.py`、`AdbStdoutCapture.ts`、`AdbResultSidechannel.ts` 处理包装器、截图和调用关联；另有登录与 Electron E2E | 同时迁移安装、工具发现、`aidb` 路由、结果回传、测试和更新工作流 |
| P03 · [sudocode](https://github.com/sudoprivacy/sudocode) | `rust/crates/plugins/bundled/browser/` 的插件及技能；图像交付约定 | 更新真实 agent 看到的说明和入口，验证模型会发现并使用新工具；不把整个引擎判作 Python 依赖 |
| P02 · [grok-web-connector](https://github.com/sudoprivacy/grok-web-connector) | `pyproject.toml` 要求 `ai-dev-browser>=0.15.0,<1.0`；使用 CDP、Tab、私有元素 API、启动补丁、BrowserPool 继承／重导出 | M1 提前做 SDK 迁移试验；W3 切换并更新下游固定版本。发布 adb 1.0 也不会自动升级此依赖 |
| P01 · [chatgpt-skill](https://github.com/elfenlieds7/chatgpt-skill) | Python 浏览器脚本、登录与 profile；另有维护中的副本 | 迁移脚本与副本，实测登录复用、查找、预览、导出和清理 |
| P04 · [sudograph](https://github.com/sudoprivacy/sudograph) | 三份 `examples/*.yaml` 的 connector 命令引用 Python 模块 | 执行并修复维护中的下载示例；现有参数存在陈旧迹象，不能仅重命名命令 |

### 3.2 受限调用方的工作分组

| 波次 | 台账 ID | 工作类型 |
|---|---|---|
| W1 | R08、R13、R16、R18、R20 | CLI 包装器、技能生成参考、预览截图、测试基础设施、上游子模块分发 |
| W3 | R02、R03、R06、R09、R12、R15、R19、R21 | SDK 脚本、复制技能、可选登录、嵌入 Python、硬编码 vendor 路径 |
| W3，M1 先评估 | R04、R05、R07、R10、R17 | 固定版本的传递依赖、深度 SDK、嵌入式浏览器、认证／profile 状态 |

文档引用共 6 个：公开的 [sudostack](https://github.com/sudoprivacy/sudostack)、[sudowork-physics-rubric](https://github.com/elfenlieds7/sudowork-physics-rubric)、[nexus](https://github.com/nexi-lab/nexus)，以及 R01、R11、R14。历史文档保留旧名称不构成运行时迁移阻塞。

### 3.3 覆盖边界

本地 worktree 按 canonical remote 合并；仓库重命名别名也合并。远端扫描覆盖当前账号可访问的组织仓库和个人名下仓库，未穷尽外部用户、未知私有仓库或所有非默认分支。nexus 的证据来自本地 worktree。构建产物、依赖目录、会话缓存、二进制和未选定文件类型被排除，7 个超过 2 MiB 的远端文件被跳过。关键词和已知传递依赖搜索可能遗漏动态导入或未知别名。

“命中引用”与“当前能运行”分别记录。若旧脚本已经使用失效符号或机器专属路径，先建旧后端基线，再决定修复、迁移或停止维护；不得把失效项目默认为已迁移。

### 3.4 完整性复核

补查扩大到当前 146 个本地工作目录，并重新检查原有 123 个远端仓库；取消源码后缀白名单，补搜 `aidb` 别名与已知传递依赖，远端检查 102,492 个文本文件。人工复核后没有新增实际调用仓库，23 个调用／分发方的分类保持不变。上文保留首轮扫描的范围与数字，便于追踪证据来源。

**这份盘点足以启动迁移，尚不足以宣布完整替换。** 61 个工具不覆盖全部 Python SDK、pool/profile、配置及消费者协议；这些接口已有单独的目录，但仍需逐项对应实现与真实验收。具体证据、排除项和待补门槛见 [覆盖范围与缺口](https://github.com/sudoprivacy/sudohand/blob/main/docs/migrations/coverage-audit.md)。

盘点时在 Windows 调试构建中实际复现了 `--help` 栈溢出，并在未修改的 main 基线 `99a7c75` 上确认同样失败。首批实现接入 PR #27 的解析器／异步命令调整后，本地调试构建的 help 和真实浏览器命令已通过；正式发行包仍需完成独立验收。

### 3.5 历史与行为盘点

补充盘点覆盖 adb v0.51.1 可达的 **305 个 commits、107 个 tags**，导出全部 **61 个工具的实际 parser**、170 个生产 Python 模块、505 个手写代码声明，以及 64 个文件中的 370 个测试定义。已通读提交标题，并复核 16 个关键提交的选定生产代码补丁；没有把全量历史索引等同于全量语义验收。

当前已形成 **27 个迁移单元**，每项关联源码、历史提交、现有测试、消费者组、main／PR #27 状态和真实验收任务。完整材料见 [历史与行为盘点](https://github.com/sudoprivacy/sudohand/blob/main/docs/migrations/history-audit.md)、[行为矩阵](https://github.com/sudoprivacy/sudohand/blob/main/docs/migrations/adb-behavior-matrix.json)及[机器清单](https://github.com/sudoprivacy/sudohand/blob/main/docs/migrations/adb-history-inventory.json)。CI 会检查清单与固定参考的一致性。

本轮最影响迁移判断的新增证据：

- PR #27 的旧参考少了 **31 个后续提交**，原 parity 结论和“有意差异”说明需要按 v0.51.1 更新。
- 真实 Chrome 的 14 次 CLI 调用确认 adb storage 存在旧方法名缺陷，PR #27 的草稿保存／刷新读取可用；应保留修复后的行为。
- 超时相关 3 个现有 live tests 全部通过，其中默认超时测试确认 JS 只执行一次；opt-in 测试未实际触发重放，仍有覆盖缺口。
- 当前 CI 显式选择了 28/50 个 integration 文件；文件名、skip 和静态参数对齐均不能代替真实任务证据。
- sudocode 使用的 noun/verb 入口出现在未合入分支；Pool 的恢复／取消、旧状态文件、已撤回接口和代码来源记录需要单独处理。

按 27 个单元推进验收后再讨论归档。本节保留盘点证据；首批运行时进展见 2.1 节，后续能力与消费者切换仍按下文阶段执行。

## 4. 接口与兼容策略

1. **先定义受支持合同。** 每个工具记录名称、参数／默认值、返回形状、stdout/stderr、退出码、重试规则、状态范围、文件产物和平台限制。与 adb 的差异要列出消费者、理由及升级方法。
2. **优先让 CLI 消费方调用新二进制。** 临时适配器可以转换命令、JSON 或 sidechannel，但必须有范围、测试和删除条件，不能吞掉失败。
3. **单独解决 Python SDK。** 一个 subprocess 包装器不能自动替代异步 CDP 对象、事件、长连接、pool 继承和持久化。M1 用真实 Grok pool 生命周期比较“改写消费者”与“有限 Python binding/bridge”，记录决策后再规模迁移。
4. **按消费者独立切换。** 先让每个集成具有明确后端选择和版本锁定，再发布可选 Rust 路径，最后改变默认。发生写入后不得自动重放到另一个后端，以免重复提交或下载。
5. **保持状态可恢复。** profile/cookie、扩展连接、端口、tab/ref 有效期、窗口状态和 pool 数据格式都要有迁移／回退说明。未经验证，不让两个后端同时写同一个 profile。
6. **收敛工具发现。** 从一个合同来源生成 `suh describe`、帮助、参数和示例；录制能力在顶层目录／描述中可发现。Rust 库与 CLI 返回保持一致；兼容层的转换单独记录。保留 `js_evaluate` 作为组合原语，避免增加重复组合命令。

## 5. 阶段与退出条件

| 阶段 | 工作与产物 | 退出条件 | 负责人角色 |
|---|---|---|---|
| M0 · 建账 | 本规划、23 个调用／分发方台账、固定 adb reference | 基线可获取；每个调用方有分类和证据；受限清单有持久存放位置 | 迁移协调人 + 各仓库维护者 |
| M1 · 合同与试验 | 61 工具合同、CLI steering 审计、共享真实 fixture、Python SDK 路线决策 | Grok pool 试验、sudowork 结果回传试验通过；每项差异有处理方式 | sudohand + Grok + sudowork 维护者 |
| M2 · 行为补齐 | 修复真实观察的差异；补录制、PDF、iframe／扩展行为；更新 parity 基线 | 当前基线的行为测试通过；关键失败无假成功；无仅靠 skip 的验收 | sudohand 维护者 + 验收负责人 |
| M3 · 可分发版本 | 跨平台发行、安装／升级／回滚说明、扩展资源、校验和、发布测试 | 从发布渠道在干净机器安装并完成真实任务；无需开发仓库绝对路径 | 发布维护者 |
| M4 · 逐仓切换 | W1 → W2 → W3；先提供可选后端，再改默认 | 每个活跃调用方有迁移 PR、发布版本、真实验收记录及回滚方法 | 各调用方维护者 |
| M5 · 稳定观察 | 新后端作为默认，重跑盘点、监测失败与产物，验证回滚 | 建议至少经历 2 个主要下游发布周期；具体窗口由维护者在 M1 确认 | 迁移协调人 + 下游维护者 |
| M6 · 归档 | 迁移公告、最终维护说明、issue 去向、安装指引、冻结旧仓库 | 第 8 节全部通过并记录最终归档决策 | 仓库维护者 |

M0 的扫描和 reference 已完成，受限台账目前仍在本地；M1–M6 尚未完成。角色不等于已经有人认领：台账里的 `owner`、`migration_pr`、`live_evidence` 在执行前逐项填写。

**实施优先级：**先做 M1 的深度依赖试验，避免 Rust 功能全部补完才发现 SDK 路线不成立。简单调用方可以先开发适配，但改变默认后端必须等待所用能力的 M2/M3 门槛。PR #27 是输入，不能按旧版本 CI 的结果直接宣布整个迁移完成。

### 5.1 三个切换波次

- **W1：**CLI、预览、CI、技能模板与示例。检验干净安装、参数、文件产物和发现路径；模板变更必须用新生成的技能实际跑通。
- **W2：**sudowork 与 sudocode。检验安装包、`aidb` 路由、截图回传及调用关联、登录填表、Electron 目标、插件默认入口和模型实际工具选择。
- **W3：**Python SDK、浏览器池、认证状态、嵌入浏览器及传递依赖。先升级上游，再显式更新固定 tag／commit 的下游；验证池恢复、并发、取消和旧版本回退。

每个活跃消费者的最终状态必须是“迁移并验收”或“明确停止维护／移除该依赖且验证剩余功能”。归档不能靠把未完成项改成“暂不处理”。

## 6. 真实验收与回归保护

沿用既定 integration-test-generator 和 cli-steering-engineering 标准：以用户任务串联有数据依赖的步骤，检查实际结果；优先通过 live PTY 运行真实二进制及真实浏览器。fixture 可控制页面和数据，浏览器/CDP/扩展/应用必须真实。

| 任务链 | 关键断言 | 执行位置 |
|---|---|---|
| 干净安装 → 启动 → 导航 → 发现 → 填写 → 提交 → 截图 | 表单保存内容、可信输入、截图可解码且包含最终状态 | Windows/macOS/Linux CI 与本地 live |
| 发现滑块 → 拖拽 → 读取页面结果 | `isTrusted`、移动时 `buttons=1`、实际位移 | 真实 Chrome 回归 |
| 设置视口 → 独立命令读取 → 截图 | 下一进程仍保持视口；图像尺寸正确 | 跨进程 live |
| 定位失败 → 按提示重新定位 → 完成任务 | 首次非零退出、稳定错误码、`hint` / `retryable`、成功后的业务状态 | CLI + flow + serve |
| 开始录制 → 导航／操作 → 停止 → 播放产物 | start/stop 跨进程可用；时长、帧内容、失败清理、已有文件保护 | 真实浏览器；发行安装 smoke |
| PDF 排版 → 导出 → 打开文件 | 纸型与单位、页数、页面尺寸、CSS 优先设置 | 真实 Chrome + PDF 读取器 |
| iframe/OOPIF 中发现 → 操作／下载 → 使用文件 | 选对 frame、真实内容／大小／哈希；嵌入式目标兼容 | CDP、扩展及 Electron/CEF live |
| 模型读取工具目录 → 自选工具 → 完成任务 → 处理失败 | 会发现录制等能力；无提示外的人为指定工具；输出真实有效 | 使用真实 API key 的本地 live 测试 |
| 安装 sudowork → agent 调用 → 图片进入会话 → 重启后复测 | 打包资源、调用关联、sidechannel／替代协议、图片展示 | 真实桌面应用与 Electron E2E |
| 登录 → pool 作业 → 下载 → 重启恢复／取消 | 会话保留、并发隔离、产物正确、没有重复副作用 | 授权账号的本地 live；可用时接受保护的 CI |

自动化步骤从实际操作经验沉淀，再加入 CI。依赖密钥／真实账户的测试先在本地实际跑过，再提交 live 测试；记录 commit、平台、命令、结果、脱敏日志和产物，不把真实 key、cookie 或个人页面写入仓库。缺少 key 的 skip 只表示未执行，不能作为验收通过。

CLI 审计要覆盖：名称与参数的单一来源、选择工具时能看到的能力范围、操作后的状态、可机器判断的错误／重试、无多余交互、取消／超时清理、stdout 与诊断流分离。`suh describe` 可列出名字不代表模型自然会使用，必须用真实 agent 任务检查。

本次只增加规划和参考资料，未声称执行了上述完整矩阵；第 2 节明确列出已做检查和未验收范围。

## 7. 发布与回滚

adb 现有正式渠道是 [PyPI](https://pypi.org/project/ai-dev-browser/) 和 [GitHub Releases](https://github.com/sudoprivacy/ai-dev-browser/releases)，GitHub 同时提供 Python 分发包和额外发行资源。依据是 [publish.yml](https://github.com/sudoprivacy/ai-dev-browser/blob/v0.51.1/.github/workflows/publish.yml) 与 [发布说明](https://github.com/sudoprivacy/ai-dev-browser/tree/v0.51.1#releases)。迁移期间保留已发布包和标签。

sudohand 的 M3 必须明确版本与平台支持，建立 GitHub Release 的可下载二进制、校验和、必要扩展资源及安装说明，验证对应 CPU 架构。若增加其他包管理渠道，另行建账；不把当前不存在的渠道写成已经发布。Python 消费者仍需 Python，改用 Rust 工具本身不会移除它们的其他 Python 依赖。

每个切换 PR 必须写明：旧后端版本、新后端版本、可选后端入口、状态／产物格式、回退命令、重试是否有副作用、测试证据。回滚要先停止新后端，保留诊断和产物，再恢复已测试的旧版本；出现不兼容状态格式时从经过验证的备份恢复。发布后至少验证一次回退流程。

## 8. adb 最终归档门槛

- [ ] 当前基线的关键功能、错误、跨调用状态和文件产物验收通过；有意改变的合同有迁移说明。
- [ ] sudohand 正式版本在支持平台可干净安装；扩展与发布资源完整。
- [ ] 23 个调用／分发方均完成维护状态判定；所有活跃依赖已迁移或明确移除，传递版本锁定也已更新。
- [ ] 每个活跃消费者有真实验收记录、CI 或已实际跑过的 live 测试，以及可执行的回滚方法。
- [ ] 已完成约定的稳定观察窗口；没有遗留的关键迁移故障。
- [ ] 对已知工作区、远端默认分支和发行配置重新扫描；剩余 adb 引用均有理由。
- [ ] 受限台账已持久保存并分配维护者；公开 reference、合同和旧版本源码仍可追踪。
- [ ] adb README、发行／安装说明、issue 路由和最终维护政策指向 sudohand；保留 PyPI 包、tags、Releases 和历史。
- [ ] 维护者记录最终归档决策，再执行 GitHub archive。

归档是最后的仓库冻结动作。完成后 sudohand 继续保留固定版本 reference，避免历史功能和迁移依据丢失。

## 9. Reference 与长期更新

`references/ai-dev-browser` 是指向公开 adb 仓库的 Git submodule，固定在 v0.51.1；[baseline.json](https://github.com/sudoprivacy/sudohand/blob/main/references/ai-dev-browser-baseline.json) 记录完整 SHA 和工具目录。它用于源码、合同和行为对照，不参与产品打包。操作见 [references/README.md](https://github.com/sudoprivacy/sudohand/blob/main/references/README.md)。

升级参考时单独提交 PR，列出上次基线后新增或改变的工具，更新 manifest、parity 检查和真实测试。保留历史 SHA，不自动追随远端 HEAD。使用任何源码／资源时保留原许可证与出处。

每次消费者发布或基线变化时更新台账的状态与证据；每次切换波次结束重新盘点。公开 JSON 是机器可读记录，受限详情由同一 ID 关联。

## 10. 文档发布与维护

本文件是规划源文件，通过 ShareOne skill 的 `--remote-url` 绑定 GitHub 内容，按 Markdown 原格式发布。正式源地址为 `main` 分支的本文件；分享记录保存在同目录 `shareone.json`。

首次发布曾使用规划分支；合并后将同一 share 绑定到 `main`，保持 share ID 和分享链接不变。后续更新源文件后，ShareOne 在访问时检查远端变更；需要立即同步时使用 skill 的 `refresh_share.js`。发布验收要实际打开分享页，检查中文标题、表格和链接。
