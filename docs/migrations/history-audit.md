# adb 历史与行为迁移盘点

日期：2026-10-01。参考：adb **v0.51.1 / c349d347**；目标 main **5743f025**；待合入 PR #27 **045202ed**。三个基线分别记录，不能把 PR 的实现记成 main 已交付。

**结论：迁移对象已经从命令目录扩展成可追溯的行为清单；目前仍不具备替换和归档条件。** 尤其不能把旧版本的缺陷、已撤回的接口、尚未合入的分支，与当前需要保留的行为混在一起。

## 1. 本次覆盖到什么程度

| 对象 | 本次覆盖 | 如何解释 |
|---|---:|---|
| 固定版本可达历史 | 305 commits，107 tags | 全量记录 SHA、日期、标题、变更路径、tag 对应提交；已通读提交标题，重点变更另读提交说明／补丁 |
| 有明确代码补丁复核记录 | 16 个提交的选定生产代码补丁 | 清单见下节；没有声称逐行审完全部 305 个提交 |
| 当前 CLI | 61 工具，398 次长参数声明 | 从真实 argparse parser 导出，含重复出现的通用参数／help；不是 398 个独立参数，也不是行为通过率 |
| 当前生产 Python 模块 | 170 | 每个模块关联迁移单元；其中 58 个位于生成 CDP 目录，61 个为工具入口 |
| 手写代码声明 | 505 | 类、函数、方法和私有成员的 AST 声明，含参数／字段；并非 505 个公开 API |
| 测试定义 | 64 文件，370 个 `test_*` 定义 | unit 14 文件／67 定义，integration 50 文件／303 定义；参数化后的 pytest item 数会不同 |
| 迁移单元 | 27 | 每项有源码、历史锚点、现有测试、消费者组、main/PR 状态和真实验收任务 |

机器清单：[adb-history-inventory.json](adb-history-inventory.json)。人工维护的任务合同：[adb-behavior-matrix.json](adb-behavior-matrix.json)。原消费者 ID 继续使用 [dependency-inventory.json](dependency-inventory.json)，受限仓库不公开名称。

机器清单包含上游帮助文本，作为参考资料保留原许可，见 [REFERENCE-NOTICE.md](REFERENCE-NOTICE.md)；不作为独立 MIT 实现或产品运行代码分发。

清单保留所有历史路径。`capabilities_by_current_path` 只是当前文件路径的关联提示：早期 `nodriver_kit/`、根目录 `tools/`、已删除文件和纯文档提交可能没有自动关联，不能据此判断提交无关或已经迁完。生成 CDP 按模块登记，尚未逐一证明每个协议命令、事件和类型的 Rust 语义一致。

### 1.1 复核过的关键补丁

以下记录的是本次实际打开的生产代码差异范围，其他历史锚点可能只复核了提交说明、路径或当前实现。

| 提交 | 已读补丁范围 | 留下的迁移约束 |
|---|---|---|
| `fdf29f5` | `_tab.py`、`_transport.py` | 超时默认不重放；显式 opt-in 才走安全重试 |
| `099f6e7` | `browser.py` | 无 profile 时强制隔离；持久化／复用必须明确指定 profile |
| `5269597` | `connection.py` | cookies 新写 JSON，旧文件仍有 pickle 读取路径；关闭要让 Chrome 刷盘 |
| `be3e078` | `cookies.py` | live cookies 返回完整值的裸列表；不能照搬旧预览截断值／包装形状 |
| `81b09ec` | `navigation.py` | `success` 表示已发出导航，`ready` 独立表示加载完成 |
| `b9b654e` | `_cli.py` | flat error 保持兼容；hard/soft failure 的退出码不同 |
| `473c9fb` | `cdp.py` | 临时 CDP 会话里的设置不能假装跨 CLI 调用持久化 |
| `9b4cb70` | `window.py`、`connection.py` | registry 记录显式窄视口，在下次调用重建 |
| `3d3de7d` | `cdp.py` | JSON 对象／数组需要转回生成绑定的类型 |
| `5e4c26f` | `elements.py` | 实际滚动前后比较；失败建议必须符合当前页面 |
| `9eaace0` | `elements.py` | `found=true, scrolled=false` 可以表示已在视野中 |
| `3e3c12c` | `_tab.py` | 拖拽移动事件必须带 held-button；同一 Tab 保存上次鼠标位置 |
| `0316df0` | `_tab.py` | OOPIF 注册稍晚，不能只查一次目标列表 |
| `ec3b215` | `cdp.py` | 枚举字符串需要绑定类型转换 |
| `a63f2ef` | `_cli.py` | bool/string 模式、None 默认值、缺少目标的恢复提示 |
| `c349d34` | `_recorder.py` | 停止录制前补采最终 viewport，避免最后一页丢失 |

所有 SHA 可在机器清单中解析为完整值；源码参考固定在 [references/ai-dev-browser](../../references/README.md)。迁移每个单元时，须从相关历史锚点继续展开与该单元有关的补丁和测试，不能把上述抽查当作全量语义签收。

## 2. 新确认的差异和容易误迁的细节

### 2.1 旧 parity 参考已经少了 31 个提交

PR #27 原本比对的是 adb v0.38.1 / `94170d23`，当前参考多出 **31 个提交**。其中包含最终 URL／ready、PDF 尺寸、iframe 下载、视口持久化、滚动和 tab_close 的真实结果、CDP 参数转换、拖拽、扩展路由和录制。

PR 中“Python 返回旧 URL”“Python tab_close 没有真正关闭”的说明，描述的是旧参考，不能再当作 v0.51.1 的现状。重新验收应固定新参考，并逐项重写这些有意差异的说明。

本次已实际重跑 PR #27 的 `scripts/cli_parity.py`，把参考换成 v0.51.1。检查 exit 1：缺少两个录制命令，以及 download_link 的 `--frame`、mouse_click/mouse_move 的 `--human-like`、page_pdf 的 `--margin`／`--paper`／`--prefer-css-page-size`。这是当前帮助声明的真实对照结果，仍不覆盖所有参数类型和行为。各项运行摘要见 [history-validation.json](history-validation.json)。

### 2.2 storage 的旧版本缺陷已用真实 Chrome 复现

这次从 live PTY 运行了两个浏览器任务，共 **14 次独立 CLI 调用**：打开页面 → 保存草稿 → 刷新 → 工具读取与页面显示交叉确认 → 关闭。

- adb v0.51.1：`storage_set/get` 均 exit 1，报不存在的 `set_local_storage/get_local_storage`；页面刷新后仍显示 `EMPTY`。
- PR #27：set/get exit 0；工具和刷新后的页面都读到 `migration-draft`。

代码：[live_adb_history_review.py](../../scripts/live_adb_history_review.py)；记录：[live-history-review.json](live-history-review.json)，含 Rust 二进制 SHA-256。它记录的是固定版本的差异，Python 的预期失败不代表能力通过。修复后的 Rust 行为应作为目标合同；adb 是否回补修复另行安排。

### 2.3 退出码不能简化成“任何 false 都失败”

当前 `_cli.py` 对带顶层 `error` 的结果给出非零退出码；只有结构化 soft failure 的字典仍可能 exit 0，同时带 `error_code/retryable/hint`。之前实测的缺失 HTML ID 点击带顶层 error，Python exit 4；不能从这个例子推导所有 `clicked=false` 都 exit 4。

同样，`page_goto` 的 `success=true, ready=false` 表示导航已发出但尚未加载完成；`page_scroll` 的 `found=true, scrolled=false` 可能表示目标已可见。迁移需要理解字段之间的关系，不能靠统一真假转换。CLI steering 验收还须覆盖原始 list/dict 形状、恢复提示、UTF-8、参数优先级、独立调用状态和真实模型工具发现。

### 2.4 Pool 和 profile 不能用命令包装替代

当前源码涉及优先／普通队列、held jobs、选择器与共享目标、business success、异常类型、重试上限、worker 增删、取消以及 checkpoint。值得单独记录：

- checkpoint 的 pending 和 in_progress 可能包含同一作业；Python 恢复时直接遍历两者，PR #27 声明有去重修正。
- Python pool 退出时取消 worker task，不能只依据“graceful”文档假定一定完成当前作业。
- `ProfileManager.shared/per_worker/temp` 管的是 **cookie 文件**，与 `browser_start(profile=...)` 的 Chrome 用户目录是两个概念。
- `test_multi_profile_pool.py` 主要验证浏览器 profile 隔离和 stop 的 event-loop 行为，不能认定它验收了 BrowserPool 作业调度。

M1 的 Grok 作业试验必须把恢复／取消／不重复副作用包括进去；子类、回调、私有元素函数和启动 monkeypatch 分别决定改写或桥接。

### 2.5 录制的 stop 是幂等的

有效 ID 的重复 stop 会返回已保存结果，并检查文件是否仍存在、大小是否改变。损坏／中断的录制不能靠重试 stop 修好；不传 ID 时按当前工作目录找唯一未完成录制。迁移不能把它简化成只在内存中的 start/stop pair。

### 2.6 来源记录需要补齐

adb 的 `LICENSE`／包元数据声明 AGPL-3.0，sudohand workspace 声明 MIT。main 的 `crates/sudohand-browser/src/elements.rs` 开头明确写了 JS snippets 是从 Python 模块逐字复制；PR #27 的独立实现说明不足以解释整个已有代码树的来源。

本次还实际比较了两边的字符串值：`_ELEMENT_INFO_INLINE`／`ELEMENT_INFO_INLINE`（772 字符）和 `_LOCATE_FOR_CLICK_JS`／`LOCATE_FOR_CLICK_JS`（889 字符）完全相同，没有做空白归一化。[核对记录](source-provenance-review.json)保留了符号、路径和 SHA-256。因此这项工作有实际代码证据，不只是依据一条注释提出的疑问。

这是一项具体的来源核查缺口，尚未作法律结论。发布前逐文件登记 JS、扩展、生成绑定、参考代码的来源与许可，并由维护者确认处理方案。把 adb 作为 submodule 固定参考，不能代替这一步。

## 3. 已撤回、改名和未合入的历史接口

| 历史接口／约定 | 历史依据 | 迁移处理 |
|---|---|---|
| `nodriver-kit` / `nodriver_kit`、旧数据目录 | `dd2a1ac` | 早期消费者可能仍用旧路径；登记状态转换，不恢复旧包名作为默认 |
| `cloudflare_verify`、`Tab.verify_cf`、`[cv]` | `50ca345` 明确撤回失效实现 | 不作为缺失 Rust 功能补回来；有消费者时改写任务 |
| `quick_connect` | `0a77ce5` 撤回 | 使用明确生命周期，单列旧调用点 |
| 仓库自带 `aidb` 启动器 | `1a34288` 撤回每次调用的 uv 开销 | 区分 adb 官方入口与 sudowork 自己维护的 wrapper |
| `cookies_extract`、`cookies_list` | `be3e078` 改名／删除 | 迁到 offline/live；必填 domain、完整值和 list 形状也要改 |
| session ID 标记、profile-prefix 判断 | `fcf0746`、后续 `5269597`／`df15c13` | 当前按 workspace/profile/GUID 约定验收，不回退到早期猜测 |
| 默认 headless | `ebea4be` 后由 `cede7ae` 撤回 | 以当前参数和环境解析为准，不凭历史功能提交恢复 |
| `list_frames` | `12a2ad6` 删除 | 按现有 frame/ref/JS 能力迁移；若需要新目录，另列需求 |
| `disable_default_args` 与旧别名 | `53dfb48`、`266a204`、`214b6d8` | 以当前生成 parser 为合同，历史消费者另做转换 |
| `browser <noun> <verb>` console script | **未合入** `e1cf4b6`，`origin/feat/browser-cli` | sudocode 技能使用了这类入口；先核对其实际安装的包装器／分支，不能当成正式 adb 包能力 |

本地已有 Git refs 还包含 6 个不在 v0.51.1 祖先链上的提交：`e1cf4b6`、`f839e56`、`a415077`、`11c7724`、`f0a6478`、`d780ec1`。它们单独作为分支历史证据，没有混入 305 个正式基线提交，也没有穷尽所有远端／已删除分支。

## 4. 27 个单元的验收目录

下表给出人读目录。每行的完整源码／测试／消费者／main 与 PR 状态保存在 [行为矩阵](adb-behavior-matrix.json)。所有行均待目标发行版本签收，存在实现或某个用例通过不等于整行通过。

| ID | 迁移单元 | 核心验收任务 |
|---|---|---|
| C01 | 启动／隔离／复用／关闭 | 两个会话互不影响，命名 profile 保留登录，失败启动无孤儿 |
| C02 | 发现／所有权／清理 | 外部浏览器保留，只有符合范围的自有进程被清理 |
| C03 | 配置／代理／语言 | 冲突参数与环境变量；实际语言、代理和复用身份一致 |
| C04 | 扩展／账号／弹窗／断开 | 真实扩展登录、多个 tab、popup、worker 重启后仍正确路由 |
| C05 | tab 选择／关闭 | URL 匹配与跨调用选择正确；关闭计数等于实际消失目标 |
| C06 | 导航／等待／dialog／focus | 重定向、慢加载、prompt 的实际结果与返回相符 |
| C07 | AX／DOM 发现与 ref | 发现结果直接驱动同源、srcdoc、OOPIF 控件 |
| C08 | 文本／ID／XPath | 重复标签、可访问名称、模糊和正则匹配的真实结果 |
| C09 | 点击与反馈 | trusted input、滚动／遮挡／祖先 fallback 和最终页面效果 |
| C10 | 输入／按键／选择 | 受控输入、多语言、替换／追加／Enter 和服务端提交值 |
| C11 | 鼠标／拖拽／滚动 | buttons 状态、最终位移、嵌套滚动与 CEF 错误边界 |
| C12 | ref 动作／上传 | 从发现到文件上传／选择／提交，验证收到的字节 |
| C13 | 页面读取／JS／frame | 深度值、console、异常、跨 frame 执行与不重放 |
| C14 | 视口／窗口 | 新 CLI 进程仍保持移动端布局，扩展拒绝部分 emulation 时可恢复 |
| C15 | 截图／坐标／图像限制 | 高 DPR 图片定位后点中目标，真实 agent 显示图像 |
| C16 | PDF | 实际纸张尺寸、页数、边距／CSS 和渲染结果 |
| C17 | 录制 | 跨进程交互，首尾帧／时长、幂等 stop、损坏检测与清理 |
| C18 | 下载 | 认证／frame／blob 下载，验证文件内容和完成状态 |
| C19 | 在线 cookies／持久化 | HttpOnly／session 登录态跨浏览器保存恢复 |
| C20 | 离线 cookies／密钥 | 各 OS 真实临时 profile、锁库、不可解密及部分读取 |
| C21 | localStorage | 保存草稿、刷新、工具与页面读到同一值 |
| C22 | 人工登录 | 测试账号登录、重开、过期恢复、取消 |
| C23 | 异步 SDK／CDP／事件 | 消费者订阅／导航／超时／重连，不重复副作用 |
| C24 | Pool | 提交／暂停／优先／重试／扩缩／重启恢复／取消 |
| C25 | ProfileManager | cookie shared/per_worker/temp 的共享、隔离与回退 |
| C26 | CLI／发现／错误／sidechannel | 真模型选择工具，正确处理失败并回传截图 |
| C27 | 安装／发行／来源 | 干净安装、发行资源、版本校验、桌面打包、升级回退 |

## 5. 状态文件与跨进程协议

| 状态／产物 | 当前源头 | 迁移／回退要求 |
|---|---|---|
| `~/.ai-dev-browser/profiles/<workspace>/<profile>` | `config.py`、`chrome.py` | 保留 workspace slug 和命名规则的转换；真实登录试验，不让两个后端同时写 |
| `cookies.dat`、`cookies/cookies_worker_N.dat`、自定义路径 | `connection.CookieJar`、`profile.py` | 扩展名不能用来判断 JSON/pickle；备份、转换和失败诊断 |
| `instances/<port>.json` | `registry.py` | port/GUID/PID/workspace/user_data_dir/identity/viewport；旧进程和复用端口不能误关联 |
| Pool checkpoint v1 | `pool/persistence.py` | completed/pending/in_progress、Job/JobResult 字段、原子写、去重和未知版本处理 |
| `recordings/<id>/` 与旁路 `.partial` | `recording.py`、`_recorder.py` | config/state/stop/abort/acknowledged/日志／锁、心跳、完成文件与跨 cwd 选择 |
| 扩展 bridge、Chrome MV3 状态 | `extension.py`、`ext_bridge.py`、`background.js` | driver、session、tab、account、daemon 断开／重连协议单独迁移 |
| output 路径与截图 metadata | `resolve_output_dir`、`_image_cap`、page/ax | `AI_DEV_BROWSER_OUTPUT_DIR` 与产物字段保持消费者可读 |
| 调用关联与结果 sidechannel | P05 消费者协议 | `AI_DEV_BROWSER_CALL_ID`、`AI_DEV_BROWSER_SIDECHANNEL_URL`、`AI_DEV_BROWSER_SIDECHANNEL_SECRET` 的协议要实测；不公开值 |

包内 16 个环境配置名保留在 [python-api-surface.json](python-api-surface.json)。本表登记了需要迁移的状态类别，不表示已经读取或转换用户现有登录数据。

## 6. 测试证据与缺口

当前 adb CI 明确选择了 **28/50** 个 integration 文件；另外 22 个没有在该工作流中被显式选择，具体文件和 mock/skip 线索在机器清单中。没有把 `integration/` 路径、文件被选中或 skip 当成真实浏览器通过。

- `ci.yml` 第一段 headless smoke 在启动失败时会打印 SKIP 并 exit 0；后面的 consumer recipe 是另一项检查，不能合并理解。
- 扩展 job 实际选择 `real_extension` 测试，使用 Chrome for Testing；与 stub bridge／内存 fake 测试分开看。
- 本轮 live PTY 重新执行 `test_timeout_and_retry.py`：**3 passed，0 skipped，14.34s**。默认超时测试实际观察计数为 1。名为 opt-in retry 的测试只对快速 `1+1` 传入参数，没有制造超时，所以还不能证明 rediscovery/replay 分支真的走通。
- 本轮新增 storage 对照脚本已在真实 Chrome 跑通。此前 39 次 CLI 行为对照继续作为独立证据，不重复计入本轮调用数。
- Windows main 调试 CLI 的 `0xc00000fd` 问题仍未解决；本轮 storage Rust 结果来自可运行的 PR #27 二进制。没有据此声称 main 或 release 构建通过。
- Grok 真实作业、sudowork 桌面安装到 agent 图像回传、原生 OS input、真实登录、模型 API key 测试及三个 OS 的完整行为均未在本轮签收。

### 6.1 重现

从 sudohand 根目录运行，Python 使用安装了参考包依赖的环境；库存快照固定用 Python 3.14，防止不同解释器格式化注解造成无意义差异：

```sh
git submodule update --init references/ai-dev-browser
python -m pip install ./references/ai-dev-browser
python scripts/audit_adb_reference.py --check
python scripts/live_adb_history_review.py --reference /path/to/adb-v0.51.1 --suh-repo /path/to/pr27 --suh /path/to/pr27/target/debug/suh --output /tmp/live-history-review.json
cd /path/to/adb-v0.51.1
python -m pytest tests/integration/test_timeout_and_retry.py -v
```

Windows 的 Rust 程序名是 `suh.exe`。先确认参考和 PR checkout 的 SHA；live 脚本会校验 SHA，但二进制本身仍须从相应 checkout 构建，运行记录保存其 hash。

新增 CI job 对完整历史、tag、实际 parser、模块映射、SDK 声明及测试目录做一致性校验。它保护的是盘点完整性，不冒充 Rust E2E。storage live 脚本按用户要求已本地实际运行后提交；未把尚未合入的 PR #27 作为 main CI 的隐式运行依赖。

## 7. 后续任务的执行顺序

| 顺序 | 工作单元 | 责任角色 | 完成证据 |
|---|---|---|---|
| M0 补齐 | C27 来源记录；P03 实际入口；私有消费者台账持久化 | 仓库／消费者维护者 | 逐文件来源结论、真实安装入口、团队可访问的私有台账 |
| M1a | C23–C25 + P02 | SDK／Grok 维护者 | 真实 pool 作业、恢复／取消记录；改写或 bridge 的决定 |
| M1b | C15/C26/C27 + P05 | sudowork 维护者 | 安装 → agent 发现 → 操作 → 图像回传 → 重启／回退 |
| M2 | C01–C22 的差异关闭 | browser 维护者 | 以 v0.51.1 为参考的任务验收，含 31 个新提交的行为 |
| M3 | C27 平台与发行 | 发行维护者 | 各平台实际二进制启动、干净安装、资源和升级回退 |
| M4–M6 | 所有消费者切换与归档 | 每仓负责人 | 每行关联目标版本、测试代码、最近运行证据及确认人 |

这些角色尚未分配到个人。迁移阶段更新每个单元的状态、运行记录和目标 SHA；有意变化必须同时迁移消费者。当前清单使遗漏更容易被发现，真正的归档门槛仍是逐项任务和下游交付闭环。
