# Agent 工具循环架构评审（归档）

> ⚠️ **归档文档，且缺失第一章**。原评审的"第一章：现有架构问题"在一次改写中
> 丢失，只保留了第二章起的结论章节。决策与实现仍以本文件为准，但"为什么要改"
> 的问题梳理部分已不可用——需要时回看 git 历史或重新梳理。

## 第二章：目标形态

### 说话是工具，思考是事件

模型**不直接写正文**。所有用户可见的话都通过 `send_message` 工具发出，一次调用
就是一条独立消息。循环只在模型本轮没有任何 `tool_calls` 时结束——那是模型在说
"我这轮说完了"。

```
Agent::chat_stream(ChatInput, on_message) -> ChatOutput
  ChatOutput {
      session_id, user_message_id,
      messages: Vec<String>   // 本轮按序投递的每条独立消息
      reply: String            // messages.join("\n")，给只需要单一文本的宿主
      memory: Option<MemoryRecord>
  }

常驻工具集（definitions() 每次请求全量下发）
  send_message { text }       -> 已发送          // 唯一出口，输出工具
  get_now {}                  -> RFC3339 + 时段  // 只读
  recall_memory { query }     -> 模型改写的检索语句 -> MemoryHit[]  // 只读

主循环（MAX_TOOL_ITER = 8 硬上限）
  loop iter:
    outcome = llm.complete_stream_with_tools(messages, tools)
    outcome.tool_calls 为空 -> 缓冲正文落库投递 -> break
    否则: 压回 assistant(含 tool_calls)
          逐个执行、按序回填 tool 结果（含 send_message 的"已发送"）
          连续 2 轮无输出则提前 break
  循环结束后 settle_memory 一次
```

### 三条不变式

1. **输出只走 `send_message`**。模型绕过工具直接写正文时，只在**整轮无工具调用**
   的前提下作为兜底接受；正文与工具同轮出现则丢弃正文（有 `content` 前言 + 工具
   正文时，用户会收到重复的突兀开场）。
2. **每个工具调用都要配对结果**。provider 要求 tool 轮必须有 tool 消息，配对
   缺失会让下一轮请求直接 400。
3. **思考永不外发**。`reasoning_content` 只用于日志与预算观测。

### 关键数据流

常规轮次：入站 → 循环 → 模型返回 1~N 个 `send_message` → 逐条投递 + 逐条入库 →
回填"已发送" → 模型不再调工具 → break → settle。

需要记忆的轮次：模型先返回 `recall_memory { query }` → 用**模型改写的 query** 检索
（不是用户原话）→ 回填 → 模型返回 `send_message`。工具只在模型主动要求时发生，
没有无条件记忆底座。

多条消息：模型在**同一次响应里并行返回多个 `send_message`**（实测"说 3 句"得到
3 条、长故事得到 22 条）。所以条数不是循环轮数，宿主按 `messages` 逐条发送、
条间插入节奏即可。

### 工具轮不产消息，用户就干等

工具轮可能只调 `get_now`，一个字的正文都没有。宿主必须自己处理等待期：
Telegram 的 `sendChatAction` 5 秒过期，需要在**每条消息发送前**刷新，而不是每轮
开始时打一次。

## 第三章：如何实现

### 删除

| 对象 | 原因 |
| --- | --- |
| `core/intent/continuation.rs` 整个模块 | 追问改由模型自己决定说几条，不再由后置状态机追加 |
| `core/intent/mod.rs` 的 `pub mod continuation` | 同上 |
| `build_recall_context`（`recall.rs`） | 召回改为工具返回，无条件注入路径整体消失 |
| `system_prompt` 的 `recall` 参数 | 提示词不再拼记忆区块 |
| `NO_MORE_TOOLS_HINT` | 实测 `finish` 类工具命中率仅 1/10；终止判据已经是"无工具调用"，不需要提示 |

### 新增

| 对象 | 说明 |
| --- | --- |
| `llm/tool.rs` | 三个常驻工具的定义、参数解析、执行分发 |
| `llm/tool_tests.rs` | dispatch 与 schema 的单测 |
| `TurnTools`（`core/agent.rs`） | 实现 `ToolContext`，把 `get_now` / `recall_memory` 接到真实记忆库 |
| `channels::pacing_delay` | 条间投递节奏（0 / 900 / 1400 / 1900ms） |
| `ChatOutput.messages` | 本轮每条独立消息；`reply` 降级为它的 join 视图 |
| `schema/message_tests.rs` | `ChatCall` / `ChatMessage` 工具字段单测 |

### 修改

| 文件 | 改动点 |
| --- | --- |
| `core/schema/message.rs` | `ChatMessage` 增 `tool_calls` / `tool_call_id` / `tool_name`；`ChatCall` 新增 |
| `llm/chat.rs` | 请求支持 tools 与 tool 消息；`capture_tool_calls` + `capture_reasoning_content`；`STREAM_MAX_TOKENS` 提到 2048；正文改为缓冲后按条件下发 |
| `core/agent.rs` | 单次调用改主循环；`MAX_TOOL_ITER` / `MAX_MUTE_ROUNDS` 守卫；每条消息独立入库 |
| `core/context/prompt.rs` | 新增"说话必须走 send_message"的输出约定；移除 continuation 相关约定 |
| `core/context/mod.rs` | 移除 `build_recall_context` 导出 |
| `telegram.rs` / `ilink/mod.rs` | 按 `messages` 逐条发送 + 节奏；每条前刷新 typing；删除 `after_reply` 调用 |
| `sa-cli.rs` | 逐条打印 `messages` |
| `api/chat.rs` | 回调语义由 delta 改为整条消息 |

### 一处容易踩的坑：正文下发时机

流式里 `content` 是逐片到达的，但**"这轮有没有工具调用"要等 `End` 才知道**。
若边收边下发，随后发现同轮还有 `send_message`，用户就会先收到一段不该看到的
前言。所以正文一律缓冲，`End` 时按 `tool_calls` 是否为空裁定下发或丢弃。

## 第四章：为何要这样设计

### 决策：正文走 send_message 工具，而不是 content

❌ 被否决的方案：`content` 非空即投递，多条靠循环反复调用自然产生。

✅ 采用：`send_message` 是唯一输出出口，终止判据是"无工具调用"。

理由来自对四家主流做法的横向对照：

- **Anthropic**：官方指引明确"循环直到 `stop_reason != tool_use`"，且把
  `text` 与 `tool_use` 并列为同一响应里的两种可能产出。
- **OpenAI Agents SDK**：产出目标类型（如 `text`）且**没有剩余 tool call** 即为
  final output，循环终止。
- **Vercel AI SDK Agent**：`stopWhen` 默认 `stepCountIs(1)`，即一次调用就是一步，
  是否继续完全交给模型。
- **Letta**：`send_message` 是唯一输出工具，`reasoning` / `assistant` /
  `tool_call` / `tool_result` / `result` 是并列事件——本方案基本照搬这套语义。

关键的实务证据：`DeepSeek-V4.1-Flash` 走 `send_message` 时，`delta.content`
出现 0 次，全部内容都在工具参数里。这说明模型**本来就更倾向于把话放进工具**，
`send_message` 不是逆着训练分布推，而是顺着。而要求它"把回答拆成多轮调用
分别写"才是真正的逆分布操作。

### 决策：终止判据是"无 tool_calls"，不加 finish 工具

❌ 被否决的方案：加一个 `finish` 工具让模型显式收尾；或依赖 `stop_reason`。

✅ 采用：本轮 `tool_calls` 为空即结束。

理由：先验方案 `finish` 在实测里**10 次只中 1 次**——不可靠的终止信号比没有更
糟，它会在漏调时把话咽回去。其余方案都有同样的软肋：Anthropic 自己也要靠
`stop_reason` 而非模型显式声明；靠"content 非空即结束"则会在模型先说一句
"我查一下"时误判收尾。`tool_calls` 为空是唯一**零歧义**信号：模型要么调用了
工具，要么没调用。

`stop_reason` 只记日志不参与判断：`StopReason` 是 `Option`，本地推理框架不报
此字段是常态，把它当判据会在字段缺失时静默走错分支。

### 决策：保留思考模式

❌ 被否决的方案：`thinking={"type":"disabled"}` 提速并压 token。

✅ 采用：保留。

理由：实测关掉思考后单轮从 82 降到 42 tokens、2.23s 降到 1.63s，看着很划算，
但这只是**单轮**数字。工具循环下每轮都要想，思考质量直接决定模型是否调对工具、
是否把话放进 `send_message` 而不是漏在 `content` 里。省下的 40 tokens 换一次
工具调用失败，代价不成比例。

### 决策：记忆纯按需召回，不保留无条件底座

✅ 采用：提示词不含记忆区块，全部召回走 `recall_memory` 工具，query 由模型改写。

理由：记忆的价值在于被问起时答得上，不是每次都塞进上下文。底座召回既占
prefill，又会把不相关的记忆暴露给模型，增加"把别人的事说成自己的"的概率。

### 决策：超长消息不截断，让模型自己拆

超限的 `send_message` 返回提示要求拆成多条，**不投递也不截断**。

理由：截断会留下半句话，而半句话比一句过长的话更像故障。模型自己知道在哪断，
硬切则会把断点定在一个它没选的位置上。

### 已知代价

- 每轮至少一次额外调用：最后一次用于确认"没有工具调用了"。纯 prefill 为主，
  成本不高但非零。
- 小模型可能停不住，所以 `MAX_TOOL_ITER = 8` 不能省。同理需要 `MAX_MUTE_ROUNDS = 2`
  防它在只读工具上来回打转却一句话不说。
- `recall_memory` 的召回质量依赖模型改写 query 的能力；改写差就等于没有记忆。
- API 侧的 SSE/WS 由"逐字增量"变成"整条消息"，前端若还在拼接增量需要改。