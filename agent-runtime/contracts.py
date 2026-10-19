"""Authoritative domain argument types. Exported JSON Schema is also checked by the native host.

工具参数（资料、开书草案）与作品状态层文档（章节卡、拍子、规则集、质量报告、平台画像、
短篇结构卡、选题卡、引导交互）都在这里定义。`export_contracts.py` 从这里导出 JSON Schema
给模型侧校验、Rust 宿主校验，并生成前端 `src/types/canon.ts`。三端共用一份来源。
"""
from typing import Annotated, Literal
from typing_extensions import TypedDict, Required
from pydantic import ConfigDict, Field, TypeAdapter, with_config

Identifier = Annotated[str, Field(min_length=1, max_length=160, description="按工具返回的原始文本编号传入，不改成数字")]
Revision = Annotated[int, Field(strict=False, ge=0)]


@with_config(ConfigDict(extra="forbid", strict=True))
class SkillSourceFile(TypedDict):
    path: Annotated[str, Field(min_length=1, max_length=255, description="技能内相对路径，如 references/evidence.md、scripts/analyze.py 或 requirements.txt")]
    content: Annotated[str, Field(max_length=262144, description="完整 UTF-8 文件内容，不是 diff")]


@with_config(ConfigDict(extra="forbid", strict=True))
class SkillContent(TypedDict, total=False):
    name: Required[Annotated[str, Field(min_length=1, max_length=80)]]
    description: Required[Annotated[str, Field(min_length=1, max_length=1000)]]
    instructions: Required[Annotated[str, Field(min_length=1, max_length=50000, description="完整 Markdown 技能说明")]]
    kind: Literal["method", "style", "check", "tool"]
    replaces: Annotated[str, Field(max_length=100, description="写作方法替换目标，如 chapter-writing；辅助技能省略")]
    files: Required[Annotated[list[SkillSourceFile], Field(max_length=64, description="全部可编辑参考资料、脚本与依赖；纯说明技能为空数组")]]

ShortText = Annotated[str, Field(strict=True, max_length=120)]
Content = Annotated[str, Field(strict=True, max_length=30000)]
Quote = Annotated[str, Field(strict=True, min_length=1, max_length=200, description="来源原文中连续存在的摘录，不改字、不拼接；宿主核对不到原文即降为待核实")]
Confidence = Annotated[float, Field(strict=False, ge=0, le=1, description="0 到 1；低于 0.7 进入待核实")]
Order = Annotated[int, Field(strict=False, ge=0)]
Strength = Annotated[int, Field(strict=False, ge=1, le=5)]
Ratio = Annotated[float, Field(strict=False, ge=0)]
RankSourceId = Annotated[int, Field(strict=False, ge=0, description="本地榜单编号；0 用于列出已保存的榜单")]
RankLimit = Annotated[int, Field(strict=False, ge=1, le=100)]
RankDate = Annotated[str, Field(strict=True, pattern=r"^(?:|\d{4}-\d{2}-\d{2})$", description="快照日期 YYYY-MM-DD；空值选择本地最新快照")]


@with_config(ConfigDict(extra="forbid", strict=True))
class RankSourceRequest(TypedDict, total=False):
    siteCode: Literal["fanqie", "qidian", "qimao"]
    gender: Literal["male", "female"]
    rankType: ShortText
    category: ShortText
    offset: Annotated[int, Field(strict=False, ge=0)]
    limit: RankLimit


@with_config(ConfigDict(extra="forbid", strict=True))
class RankGetRequest(TypedDict, total=False):
    sourceId: Required[Annotated[int, Field(strict=False, gt=0, description="能力目录中的榜单编号")]]
    freshness: Required[Annotated[Literal["latest", "cached", "historical"], Field(description="latest 获取最新可用榜单，必要时联网刷新；cached 仅本地；historical 精确读取 statDate 指定日期")]]
    statDate: RankDate
    limit: RankLimit


@with_config(ConfigDict(extra="forbid", strict=True))
class RankCollectionRequest(TypedDict, total=False):
    """范围查询；筛选条件只收窄采集范围，展示数量不限制分类数量。"""
    siteCode: Required[Annotated[Literal["fanqie", "qidian", "qimao"], Field(description="作者要看的平台；作者没指定时这不是可以随便补的默认值")]]
    rankType: Required[Annotated[str, Field(strict=True, min_length=1, max_length=120, description="目录 filters.rankTypes 中的榜型值")]]
    gender: Annotated[Literal["male", "female"], Field(description="省略表示所有频道；仅请求明确限定频道时填写")]
    category: Annotated[ShortText, Field(description="省略表示该范围的全部分类，不依据浏览偏好或已有缓存缩小范围")]
    freshness: Annotated[Literal["latest", "cached", "historical"], Field(default="latest", description="默认 latest，按频控刷新整个范围，没抓过的来源要联网，常需几分钟；cached 只读本地快照；historical 读取指定历史日期")]
    statDate: RankDate
    limit: Annotated[RankLimit, Field(description="汇总展示总条数，默认20，在不同指标口径组间分配；不限制采集的分类数量")]
    perCategoryLimit: Annotated[int, Field(strict=False, ge=1, le=10, description="每分类预览条数，默认3；完整采集结果保存在快照库，可用 get_rank 读取")]


# ---------------------------------------------------------------------------
# 资料工具字段
# ---------------------------------------------------------------------------
@with_config(ConfigDict(extra="forbid", strict=True))
class CharacterFields(TypedDict, total=False):
    name: Annotated[ShortText, Field(pattern=r"\S", title="姓名")]
    role: Annotated[int, Field(strict=False, ge=0, le=3, title="角色定位", description="0主角，1主要配角，2反派，3路人")]
    gender: Annotated[int, Field(strict=False, ge=0, le=2, title="性别", description="0女，1男，2其他")]
    age: Annotated[ShortText, Field(title="年龄", description='年龄描述文本，例如 "20岁"、"二十多岁"；未知时省略，不能传数字或 null')]
    appearance: Annotated[Content, Field(title="外貌")]
    personality: Annotated[Content, Field(title="性格")]
    background: Annotated[Content, Field(title="背景")]
    ability: Annotated[Content, Field(title="能力")]
    tags: Annotated[list[ShortText], Field(max_length=30, title="标签")]
    aliases: Annotated[list[ShortText], Field(max_length=20, title="别名", description="正文中出现的其他称呼，索引器据此解析人物；不含本名")]
    voice: Annotated[Content, Field(title="语言指纹", description="口头禅、语气、句长偏好、禁用词、代表台词；生成对白与审稿时使用")]


@with_config(ConfigDict(extra="forbid", strict=True))
class OutlineFields(TypedDict, total=False):
    title: Annotated[ShortText, Field(pattern=r"\S", title="标题")]
    content: Annotated[Content, Field(title="大纲内容")]
    parentId: Annotated[Identifier, Field(title="上级目录", description='根目录传 "0"')]
    nodeType: Annotated[int, Field(strict=False, ge=0, le=1, title="大纲类型", description="0目录，1内容")]


@with_config(ConfigDict(extra="forbid", strict=True))
class WorldFields(TypedDict, total=False):
    name: Annotated[ShortText, Field(pattern=r"\S", title="设定名称")]
    type: Annotated[int, Field(strict=False, ge=0, le=5, title="设定类型", description="0地理，1势力，2功法，3物品，4等级，5其他")]
    detail: Annotated[Content, Field(title="设定内容")]


@with_config(ConfigDict(extra="forbid", strict=True))
class ChapterSource(TypedDict):
    type: Literal["chapter"]
    bookId: Identifier
    chapterId: Identifier


@with_config(ConfigDict(extra="forbid", strict=True))
class MaterialSource(TypedDict):
    type: Literal["material"]
    bookId: Identifier
    materialKind: Literal["character", "outline", "world_setting"]
    entityId: Annotated[int, Field(strict=False)]


@with_config(ConfigDict(extra="forbid", strict=True))
class AuthorSource(TypedDict):
    type: Literal["author"]
    bookId: Identifier
    sessionId: Identifier
    messageId: Identifier


@with_config(ConfigDict(extra="forbid", strict=True))
class ArtifactSource(TypedDict):
    type: Literal["artifact"]
    bookId: Identifier
    artifactId: Identifier


@with_config(ConfigDict(extra="forbid", strict=True))
class Replacement(TypedDict):
    id: Identifier
    revision: Revision


MATERIAL_TYPES = {"character": CharacterFields, "outline": OutlineFields, "world_setting": WorldFields}


@with_config(ConfigDict(extra="forbid", strict=True))
class PlanDecision(TypedDict, total=False):
    """内容来源：author 为作者明确表达，assumed 为作者委托 AI 拟定，suggested 为尚未确认的 AI 建议。不能把建议记成作者决定。"""
    value: Required[ShortText]
    by: Required[Literal["author", "assumed", "suggested"]]


@with_config(ConfigDict(extra="forbid", strict=True))
class PlanDecisions(TypedDict, total=False):
    """八步开书中的作者决定。已回答的直接记录；明确委托才标 assumed，未确认建议不能代替作者选择。"""
    genre: Annotated[PlanDecision, Field(title="类型")]
    platform: Annotated[PlanDecision, Field(title="平台和读者", description="发在哪、写给谁，如“番茄，男频”")]
    hook: Annotated[PlanDecision, Field(title="故事方向", description="兼容已记录的故事选择；不另设卖点选择题")]
    protagonistName: Annotated[PlanDecision, Field(title="主角姓名")]
    protagonistGoal: Annotated[PlanDecision, Field(title="主角行动目标", description="第3步由作者选择或明确委托的具体行动结果；原因属于人物动机")]
    narration: Annotated[PlanDecision, Field(title="叙述人称", description="兼容旧方案；第3步确认，新方案放入 writingPreferences.narration")]
    openingDirection: Annotated[PlanDecision, Field(title="开篇选择", description="第6步由作者选择或明确委托的开篇方案；已经选过的直接沿用")]


@with_config(ConfigDict(extra="forbid", strict=True))
class PlanNarration(TypedDict):
    """人称只表示叙述者形式，限知或全知等信息放在 viewpoint，不拼在人称里。"""
    value: Literal["第一人称", "第三人称"]
    by: Literal["author", "assumed", "suggested"]


@with_config(ConfigDict(extra="forbid", strict=True))
class PlanWritingPreferences(TypedDict, total=False):
    """人称与视角分别记录。叙述人称在第3步由作者选择或明确委托，不自动默认第三人称；已有作品沿用设置。"""
    narration: Annotated[PlanNarration, Field(title="叙述人称")]
    viewpoint: Annotated[PlanDecision, Field(title="叙述视角", description="叙述跟随谁、读者可以知道哪些信息；按故事需要拟定")]
    tone: Annotated[PlanDecision, Field(title="故事基调", description="轻松、严肃、黑暗等情绪倾向；与题材分类分开")]
    legacyNarrationNote: Annotated[Content, Field(description="旧方案原有叙述说明的无损保留，新方案不要填写")]


@with_config(ConfigDict(extra="forbid", strict=True))
class PlanCharacter(TypedDict, total=False):
    """建书时写进人物表的人物；主角放第一个，重要配角一两个即可。"""
    name: Required[ShortText]
    role: Required[Literal["主角", "配角", "反派"]]
    summary: Required[Annotated[Content, Field(description="身份与处境")]]
    goal: Annotated[ShortText, Field(description="行动目标：当前或近期希望完成的具体事情")]
    motivation: Annotated[Content, Field(description="人物动机：为什么要做这件事")]
    longTermGoal: Annotated[ShortText, Field(description="长期目标：全书或较长阶段希望达成的结果，可待定")]
    personality: Annotated[Content, Field(description="性格与缺点")]
    ability: Annotated[Content, Field(description="特殊能力、它的规则与限制")]


@with_config(ConfigDict(extra="forbid", strict=True))
class PlanSetting(TypedDict, total=False):
    """建书时写进作品资料的世界设定；只写开头会用到的。"""
    name: Required[ShortText]
    type: Literal["地理", "势力", "功法", "物品", "等级", "其他"]
    detail: Required[Content]


@with_config(ConfigDict(extra="forbid", strict=True))
class BookPlan(TypedDict, total=False):
    """开书方案包含题材定位、故事构思、人物设定、世界观设定、剧情大纲、开篇章纲、书名与简介；最后创建作品。
    按八步逐步推进，已回答的沿用。progress.current 是首个未完成步骤，blocking 列出未定决定；新书需完成作者选择或明确委托后再创建，授权与版本另行核验。"""
    workingTitle: Required[Annotated[str, Field(max_length=120, title="书名", description="准备阶段可留空；建书前必须有，由作者在备选里挑或自拟后写入")]]
    writingMode: Literal["explore", "outline"]
    profileId: Annotated[Identifier, Field(description="作者选择的写作画像编号，从 list_writing_profiles 获取；未知时省略，不能把已明确的平台只写在说明里")]
    audience: Annotated[ShortText, Field(description="本书已确定的读者群，如男频、女频或不限")]
    styleRequirements: Annotated[Content, Field(description="作者对本书的具体文风要求，随开书方案创建时传给生成、修订与评审")]
    styleRequirementQuote: Annotated[Content, Field(description="styleRequirements 的作者原话依据，必须逐字来自作者消息；没有依据的风格建议使用 styleSuggestions")]
    styleSuggestions: Annotated[Content, Field(description="AI 或平台提供的文风建议，可调整，不作为作者强制要求")]
    decisions: Annotated[PlanDecisions, Field(title="选择记录与内容来源")]
    writingPreferences: PlanWritingPreferences
    intro: Annotated[Content, Field(title="简介", description="读者在书城看到的简介，20–600 字；建书时随书名一起写进作品，开书阶段不要另存简介候选")]
    premise: Annotated[Content, Field(title="故事构思", description="主角身份、核心设定、起始事件与主要阻力组成的完整故事方案；选过故事就沿用，不重新拆成选择题")]
    sellingPoint: Annotated[Content, Field(title="核心卖点", description="这份故事方案区别于同类作品的吸引点，是方案说明，不是需要另选的方向")]
    protagonist: Annotated[Content, Field(title="主角说明", description="给作者看的主角说明；要写进人物表的人物用 characters，没定的姓名不能冒充作者决定")]
    conflict: Annotated[Content, Field(title="主要冲突", description="谁或什么阻碍主角达成目标；与特殊能力的规则和代价分开说明")]
    characters: Annotated[list[PlanCharacter], Field(max_length=8, title="人物", description="主角放第一个；建书时写进人物表")]
    world: Annotated[list[PlanSetting], Field(max_length=12, title="世界观设定", description="社会背景、势力关系及题材需要的力量体系；只写开篇所需内容，不强制特殊能力")]
    outline: Annotated[Content, Field(title="剧情大纲", description="把已选故事展开为主线、阶段发展与关键事件；不重新选故事，不强制确定结局")]
    opening: Annotated[Content, Field(title="开篇章纲", description="按第6步选定或委托拟定的开篇方案展开前三章的事件、人物选择与衔接")]
    unresolved: Annotated[list[ShortText], Field(max_length=30, title="未定事项", description="可继续完善的内容，不自动转成必答问题")]


def material_schemas():
    return {kind: {**TypeAdapter(model).json_schema(), "minProperties": 1} for kind, model in MATERIAL_TYPES.items()}


# ---------------------------------------------------------------------------
# 作品状态层：章节卡及其组成
# ---------------------------------------------------------------------------
@with_config(ConfigDict(extra="forbid", strict=True))
class EntityRef(TypedDict, total=False):
    """人物、地点、物品、势力的引用。id 解析不到时省略，宿主放入待登记列表，不以文本名入账。"""
    kind: Required[Literal["character", "location", "item", "faction"]]
    id: Annotated[Identifier, Field(description="人物表或实体表中的编号；解析不到时省略")]
    name: Required[ShortText]


@with_config(ConfigDict(extra="forbid", strict=True))
class Event(TypedDict, total=False):
    eventId: Required[Identifier]
    summary: Required[ShortText]
    characters: Required[Annotated[list[EntityRef], Field(max_length=20)]]
    location: EntityRef
    storyOrder: Required[Annotated[Order, Field(description="故事发生顺序；倒叙、回忆按实际发生时间填写")]]
    isFlashback: Required[bool]
    isMisdirection: Required[Annotated[bool, Field(description="伪装、误导、假死等后文会推翻的表象")]]


@with_config(ConfigDict(extra="forbid", strict=True))
class StateChange(TypedDict, total=False):
    character: Required[EntityRef]
    field: Required[Literal["realm", "location", "item", "relation", "goal", "alive"]]
    before: ShortText
    after: Required[ShortText]
    aliveAfter: Annotated[Literal["alive", "dead", "unknown"], Field(
        description="field=alive 时填写：这次变化之后此人是否还活着。假死、濒死、重伤、被封印都算 alive；正文没写清就填 unknown")]
    storyOrder: Required[Order]
    eventId: Annotated[Identifier, Field(description="所属事件；伪装、误导事件里的变化不参与状态折叠")]
    quote: Required[Quote]
    confidence: Required[Confidence]


@with_config(ConfigDict(extra="forbid", strict=True))
class Fact(TypedDict, total=False):
    factId: Required[Annotated[Identifier, Field(description="首次出现时分配，后文引用同一编号；修正旧事实用 supersedes")]]
    text: Required[ShortText]
    quote: Required[Quote]
    supersedes: Identifier


@with_config(ConfigDict(extra="forbid", strict=True))
class KnowledgeEntry(TypedDict, total=False):
    character: Required[EntityRef]
    factId: Required[Identifier]
    learnedAt: Required[Annotated[Order, Field(description="得知该事实的故事顺序")]]
    viaNarration: Required[Annotated[bool, Field(description="true 表示只有读者通过叙述得知，人物本身并不知道")]]
    eventId: Identifier
    quote: Required[Quote]


@with_config(ConfigDict(extra="forbid", strict=True))
class ThreadTouch(TypedDict, total=False):
    kind: Required[Literal["foreshadow", "suspense", "promise", "payoffSetup", "romance"]]
    action: Required[Literal["plant", "advance", "payoff"]]
    threadId: Annotated[Identifier, Field(description="advance / payoff 必须引用已有线索；plant 留空由宿主分配")]
    text: Required[ShortText]
    quote: Required[Quote]


HookType = Literal["悬念", "危机", "反转", "期待", "情绪", "信息", "关系"]
PayoffType = Literal["打脸", "装逼", "升级", "收获", "认可", "逆袭", "报复", "掌控", "越级", "情感", "团宠"]


@with_config(ConfigDict(extra="forbid", strict=True))
class HookSpec(TypedDict, total=False):
    position: Required[Literal["opening", "mid", "end"]]
    type: Required[HookType]
    text: Required[ShortText]
    strength: Strength


@with_config(ConfigDict(extra="forbid", strict=True))
class PayoffSpec(TypedDict, total=False):
    type: Required[PayoffType]
    setupRef: Annotated[ShortText, Field(description="铺垫来源：线索编号或前文章节与摘录")]
    strength: Required[Strength]
    amplified: Required[Annotated[bool, Field(description="是否写了旁观者反应放大")]]
    quote: Quote


@with_config(ConfigDict(extra="forbid", strict=True))
class EmotionCurve(TypedDict):
    open: ShortText
    peak: ShortText
    close: ShortText


@with_config(ConfigDict(extra="forbid", strict=True))
class StoryTime(TypedDict, total=False):
    label: Required[ShortText]
    order: Required[Order]


@with_config(ConfigDict(extra="forbid", strict=True))
class ChapterCard(TypedDict, total=False):
    """一章的结构化摘要。来源版本、失效状态由宿主记录，不在文档内。"""
    oneLine: Required[ShortText]
    narrativeOrder: Required[Annotated[Order, Field(description="叙述顺序，即目录位置")]]
    events: Required[Annotated[list[Event], Field(max_length=30)]]
    stateChanges: Required[Annotated[list[StateChange], Field(max_length=60)]]
    facts: Required[Annotated[list[Fact], Field(max_length=40)]]
    knowledge: Required[Annotated[list[KnowledgeEntry], Field(max_length=60)]]
    threads: Required[Annotated[list[ThreadTouch], Field(max_length=30)]]
    hooks: Required[Annotated[list[HookSpec], Field(max_length=10)]]
    payoffs: Required[Annotated[list[PayoffSpec], Field(max_length=10)]]
    emotion: Required[EmotionCurve]
    storyTime: StoryTime


# ---------------------------------------------------------------------------
# 写作方法：场景要点与规则集
# ---------------------------------------------------------------------------
SceneKind = Literal["开章", "对白", "打斗", "心理", "过渡", "收尾"]


@with_config(ConfigDict(extra="forbid", strict=True))
class Scene(TypedDict, total=False):
    """一个场景的要点：自然语言，写给接着往下写的人看；不写数字目标，不写写法规则。"""
    no: Required[Annotated[int, Field(strict=False, ge=1, le=6)]]
    kind: Required[Annotated[SceneKind, Field(description="场景类型；宿主按它挑同类型的文风范例给写作模型")]]
    points: Required[Annotated[Content, Field(description="谁在哪、要发生什么、以什么收，三五句话；只说事，不说怎么写")]]
    characters: Annotated[list[ShortText], Field(max_length=8, description="出场人物名；宿主据此附上他们的当前状态")]


@with_config(ConfigDict(extra="forbid", strict=True))
class ScenePlan(TypedDict, total=False):
    """一章的场景安排：按叙事需要分场景，不按固定数量凑场景。"""
    rulesetId: Required[Identifier]
    chapterGoal: Required[Annotated[Content, Field(description="这一章要让读者记住的一件事，一两句")]]
    scenes: Required[Annotated[list[Scene], Field(min_length=1, max_length=6)]]
    expectation: Annotated[ShortText, Field(description="章末留给读者的期待，自然语言")]
    infoRelease: Annotated[list[ShortText], Field(max_length=10, description="本章释放的设定或信息")]
    threadsTouched: Annotated[list[ThreadTouch], Field(max_length=20)]


@with_config(ConfigDict(extra="forbid", strict=True))
class Ruleset(TypedDict, total=False):
    """一次写作任务解析出的规则集快照；规划、生成、评审必须传同一编号。"""
    rulesetId: Required[Identifier]
    bookId: Required[Identifier]
    profileId: Required[Identifier]
    skillId: Identifier
    styleSkillId: Annotated[str, Field(max_length=160, description="本次选定的文风技能；空字符串明确不使用文风技能")]
    ruleIds: Required[Annotated[list[Identifier], Field(max_length=200)]]
    authorInstruction: Content
    fingerprint: Required[ShortText]


# ---------------------------------------------------------------------------
# 质量门
# ---------------------------------------------------------------------------
IssueLevel = Literal["block", "suspect", "notice"]
RepairMode = Annotated[Literal["prose_only", "draft_edit", "author_decision", "none"], Field(description="prose_only 为语言修订；draft_edit 为不改变已有事实、主线与本章目标的草稿返工；author_decision 为会改变作者已定事实、增加剧情事件或重大取舍；不成立或仅观察用 none")]


@with_config(ConfigDict(extra="forbid", strict=True))
class RepairRange(TypedDict):
    start: Order
    end: Annotated[Order, Field(description="含末段；连续修订单元必须含该问题至少一处有效引文定位")]


@with_config(ConfigDict(extra="forbid", strict=True))
class QualityIssue(TypedDict, total=False):
    id: Required[Identifier]
    source: Required[Literal["rule", "critic"]]
    code: Required[ShortText]
    dimension: Required[ShortText]
    level: Required[Annotated[IssueLevel, Field(description="block 只用于结构与数据完整性问题；语义判断只能是 suspect 或 notice")]]
    message: Required[Content]
    paragraphs: Required[Annotated[list[Order], Field(max_length=50)]]
    quotes: Required[Annotated[list[Quote], Field(max_length=10, description="suspect 至少两处：状态来源与本章原文")]]
    fix: Content
    metrics: dict[str, Ratio]
    observation: Annotated[bool, Field(description="程序检测到的模式线索，尚不构成阅读问题；需要结合语境复核")]
    readerEffect: Annotated[Content, Field(description="具体如何打断理解、削弱人物可信度或消耗阅读期待")]
    impact: Literal["major", "minor"]
    repairMode: RepairMode
    evidenceStatus: Literal["verified", "unlocated"]
    repairRanges: Annotated[list[RepairRange], Field(max_length=10, description="需要联动改写时列出最小完整叙述单元，包含引文段及必须一起调整的邻段，避免只改单句造成上下文重复或断裂；无需联动可省略")]


@with_config(ConfigDict(extra="forbid", strict=True))
class QualityMetrics(TypedDict, total=False):
    """程序量出来的数字：结构计数与文风指纹。只给人看，不进提示词，不作关卡。"""
    words: Required[Order]
    paragraphs: Required[Order]
    repeatedParagraphs: Required[Order]
    estimatedTokens: Required[Order]
    avgParagraphLen: Ratio
    paragraphLenStd: Ratio
    dialogueRatio: Ratio
    sentenceLenStd: Ratio
    fingerprint: Annotated[dict[str, Ratio], Field(description="文风指纹：句长、句长变异、长短句占比、段长、单句段、段末短句、对白占比、各类标点每千字、比喻词每千字、“的”每百字")]


@with_config(ConfigDict(extra="forbid", strict=True))
class StyleDriftItem(TypedDict, total=False):
    key: Required[ShortText]
    label: Required[ShortText]
    value: Required[Annotated[float, Field(strict=False)]]
    baseline: Required[Annotated[float, Field(strict=False)]]
    ratio: Annotated[float, Field(strict=False, description="相对偏差，(value-baseline)/baseline")]
    flagged: Required[bool]


@with_config(ConfigDict(extra="forbid", strict=True))
class StyleDrift(TypedDict, total=False):
    """生成稿指纹与文风基线的偏移：温度计，只在界面提示，不拦保存，不进评审。"""
    baseline: Required[ShortText]
    items: Required[Annotated[list[StyleDriftItem], Field(max_length=30)]]
    flagged: Required[Order]
    note: Content


@with_config(ConfigDict(extra="forbid", strict=True))
class RevisionAssessment(TypedDict):
    progress: Literal['improved', 'unchanged', 'regressed']
    factsPreserved: bool
    withinScope: bool
    reason: Content
    resolvedIssueIds: list[Identifier]


@with_config(ConfigDict(extra="forbid", strict=True))
class CriticSummary(TypedDict, total=False):
    """编辑评审的结果摘要：只有结论与问题，没有评分。"""
    status: Required[Literal["available", "partial", "unavailable", "not_run", "failed"]]
    errorCode: ShortText
    retryable: bool
    persona: ShortText
    summary: Content
    error: ShortText
    revisedIssueIds: Annotated[list[Identifier], Field(max_length=60, description="兼容旧报告：仅表示曾尝试修订，不表示问题已解决")]
    revisionAssessment: RevisionAssessment


@with_config(ConfigDict(extra="forbid", strict=True))
class DeliveryStep(TypedDict, total=False):
    revision: Required[Revision]
    action: Required[Literal["review", "revise", "restore"]]
    reason: Required[Content]
    issueIds: list[Identifier]


@with_config(ConfigDict(extra="forbid", strict=True))
class RequestBudget(TypedDict):
    callsLeft: Order
    callsUsed: Order
    secondsLeft: Annotated[float, Field(ge=0)]


@with_config(ConfigDict(extra="forbid", strict=True))
class DeliveryStatus(TypedDict, total=False):
    status: Required[Literal["reviewing", "revising", "ready", "awaiting_author", "budget_exhausted", "failed", "needs_attention"]]
    revision: Required[Revision]
    rounds: Required[Order]
    reason: Required[Content]
    remainingIssueIds: Required[list[Identifier]]
    history: Required[list[DeliveryStep]]
    operationId: Identifier
    runId: Identifier
    fingerprint: ShortText
    budget: RequestBudget


@with_config(ConfigDict(extra="forbid", strict=True))
class QualityReport(TypedDict, total=False):
    """绑定到具体成果版本的体检报告；成果内容变化后由宿主标记过期。"""
    version: Required[Literal[1]]
    artifactId: Required[Identifier]
    reviewedRevision: Required[Revision]
    reviewedFingerprint: Required[ShortText]
    rulesetId: Required[Identifier]
    chapterId: Identifier
    profileId: Required[Identifier]
    issues: Required[Annotated[list[QualityIssue], Field(max_length=200)]]
    metrics: Required[QualityMetrics]
    critic: CriticSummary
    writingBasisFingerprint: ShortText
    styleDrift: StyleDrift


@with_config(ConfigDict(extra="forbid", strict=True))
class GatePolicy(TypedDict):
    block: Annotated[list[ShortText], Field(max_length=50)]
    suspect: Annotated[list[ShortText], Field(max_length=50)]
    notice: Annotated[list[ShortText], Field(max_length=50)]


@with_config(ConfigDict(extra="forbid", strict=True))
class PlatformProfile(TypedDict, total=False):
    id: Required[Identifier]
    name: Required[ShortText]
    chapterWords: Required[Annotated[list[Order], Field(min_length=2, max_length=2, description="[下限, 上限]")]]
    paragraphMaxChars: Required[Order]
    endHookRequired: Required[bool]
    openingHookWithin: Required[Annotated[Order, Field(description="开篇多少字内应出现冲突或悬念")]]
    payoffEvery: Required[Annotated[Order, Field(description="每多少章至少一个小爽点")]]
    midPayoffEvery: Required[Order]
    suppressMaxChapters: Required[Order]
    infoReleaseLevel: Required[Literal["low", "mid", "high"]]
    readerPersona: Required[Content]
    gate: Required[GatePolicy]


# ---------------------------------------------------------------------------
# 短篇结构卡与选题卡
# ---------------------------------------------------------------------------
@with_config(ConfigDict(extra="forbid", strict=True))
class StoryTopic(TypedDict):
    genre: ShortText
    emotionCore: ShortText
    contrast: ShortText
    logline: ShortText


@with_config(ConfigDict(extra="forbid", strict=True))
class Mainline(TypedDict):
    want: ShortText
    obstacle: ShortText


@with_config(ConfigDict(extra="forbid", strict=True))
class Twist(TypedDict):
    readerThinks: ShortText
    actually: ShortText


@with_config(ConfigDict(extra="forbid", strict=True))
class Ending(TypedDict, total=False):
    type: Required[Literal["爽", "虐", "暖"]]
    afterTaste: ShortText


@with_config(ConfigDict(extra="forbid", strict=True))
class NameCandidate(TypedDict, total=False):
    name: Required[Annotated[str, Field(strict=True, min_length=1, max_length=30)]]
    meaning: Annotated[str, Field(strict=True, max_length=120, description="寓意、出处或为什么适合；不充当全网重名或市场效果证明")]
    readerPromise: Annotated[str, Field(strict=True, min_length=1, max_length=160, description="书名必填：目标读者从这个名字能预期什么具体看点")]
    storyBasis: Annotated[str, Field(strict=True, min_length=1, max_length=200, description="书名必填：当前作品中支撑这个名字的已知设定或情节；暂定假设要说明")]
    tradeoff: Annotated[str, Field(strict=True, min_length=1, max_length=160, description="书名必填：这个命名方向的不足、可能误读或依赖简介补充的地方")]


@with_config(ConfigDict(extra="forbid", strict=True))
class ShortStoryPlan(TypedDict, total=False):
    rulesetId: Required[Identifier]
    topic: Required[StoryTopic]
    openingLines: Required[Annotated[list[ShortText], Field(min_length=1, max_length=3)]]
    mainline: Required[Mainline]
    twists: Required[Annotated[list[Twist], Field(max_length=8, description="按作者与选定方法安排反转，不要求固定数量；无需反转时可留空")]]
    ending: Required[Ending]
    infoOrder: Required[Annotated[list[ShortText], Field(max_length=20)]]
    styleSampleIds: Annotated[list[Identifier], Field(max_length=2, description="从 read_style_profile 选择的表达示例编号；示例不是本故事事实")]


@with_config(ConfigDict(extra="forbid", strict=True))
class RankEvidence(TypedDict, total=False):
    """数据证据只指明依据哪份快照、哪几本书。名次、指标由宿主从快照读出并展示，模型不转述数字，
    因此“已核对”的内容不依赖对模型文字的理解。"""
    kind: Required[Literal["data"]]
    sourceId: Required[Annotated[int, Field(strict=False, gt=0, description="榜单编号，取自榜单工具返回的 source.id 或 sourceId")]]
    snapshotDate: Required[Annotated[str, Field(strict=True, pattern=r"^\d{4}-\d{2}-\d{2}$", description="快照日期，取自返回的 snapshot.statDate")]]
    books: Annotated[list[Annotated[str, Field(strict=True, min_length=1, max_length=120)]], Field(
        max_length=5, description="相关上榜书名，照抄工具返回的 bookTitle；不填表示引用整份快照")]


@with_config(ConfigDict(extra="forbid", strict=True))
class JudgmentEvidence(TypedDict, total=False):
    """模型自己的归纳、比较、统计与题材判断，界面标为“判断”，不作核对。"""
    kind: Required[Literal["judgment"]]
    fact: Required[Annotated[ShortText, Field(min_length=1)]]
    source: Required[Annotated[ShortText, Field(min_length=1, description="依据，如“对上述快照的归纳”“作者画像”“拆书资料”")]]


Evidence = RankEvidence | JudgmentEvidence


@with_config(ConfigDict(extra="forbid", strict=True))
class SelectionCard(TypedDict, total=False):
    genre: Required[ShortText]
    sellingPoint: Required[Content]
    protagonist: Required[Content]
    opening: Required[Content]
    benchmarks: Required[Annotated[list[ShortText], Field(max_length=10)]]
    risks: Required[Annotated[list[ShortText], Field(max_length=10)]]
    evidence: Required[Annotated[list[Evidence], Field(max_length=20, description="data 只引用快照与书名，数据由宿主读出；judgment 写模型的归纳与判断")]]


# ---------------------------------------------------------------------------
# 拆书 2.0 与仿书
# ---------------------------------------------------------------------------
@with_config(ConfigDict(extra="forbid", strict=True))
class RecipeStep(TypedDict):
    position: Annotated[str, Field(strict=True, min_length=1, max_length=40, description="章位置或阶段，如“第1章”“第4–10章”")]
    tasks: Annotated[str, Field(strict=True, min_length=1, max_length=600,
                                description="这一段要完成的写法任务，用“主角”“对手”等角色位置，不写参考书人名与具体情节")]


@with_config(ConfigDict(extra="forbid", strict=True))
class ComparisonRow(TypedDict):
    dimension: Annotated[str, Field(strict=True, min_length=1, max_length=40, description="借鉴维度：节奏、开篇、人设立法、信息释放、换地图、文风等")]
    benchmark: Annotated[Content, Field(description="对标书在这个维度的做法")]
    mine: Annotated[Content, Field(description="作者作品准备怎么做，必须来自作者自己的人物、设定与冲突")]
    difference: Annotated[Content, Field(description="和对标书的差异与理由，说明不是换名照搬")]


# ---------------------------------------------------------------------------
# 引导交互
# ---------------------------------------------------------------------------
@with_config(ConfigDict(extra="forbid", strict=True))
class PromptOption(TypedDict, total=False):
    id: Required[ShortText]
    title: Required[Annotated[ShortText, Field(description="选中即可表达答案的具体内容；已有候选时使用候选名称，不用需要再填编号的操作方式代替答案")]]
    description: Content
    preview: Content


@with_config(ConfigDict(extra="forbid", strict=True))
class PromptField(TypedDict, total=False):
    key: Required[ShortText]
    label: Required[ShortText]
    type: Required[Literal["text", "textarea", "number", "select"]]
    required: Required[Annotated[bool, Field(description="作者不填就没法往下做时才设 true；问题里说了可以留空的一律 false")]]
    placeholder: ShortText
    options: Annotated[list[ShortText], Field(max_length=30)]
    default: ShortText


@with_config(ConfigDict(extra="forbid", strict=True))
class PromptStep(TypedDict, total=False):
    """分步问答里的一问：界面一次只显示一问。作者可以点建议选项、自己写，或跳过交给 Agent 先给建议。"""
    key: Required[ShortText]
    question: Required[ShortText]
    hint: Annotated[ShortText, Field(description="一句话说明为什么问、答到什么程度")]
    options: Annotated[list[PromptOption], Field(max_length=6, description="2–4 个建议答案，贴合这本书已知的题材与设定")]
    multiple: Annotated[bool, Field(description="可以同时选多项")]
    required: Required[Annotated[bool, Field(description="作者不答就没法往下做时才设 true，其余 false")]]
    placeholder: ShortText


@with_config(ConfigDict(extra="forbid", strict=True))
class AuthorPrompt(TypedDict, total=False):
    """需要作者决定时的引导卡。choice 单选、multi 多选、steps 分步问答、form 多字段表单、compare 并排两到四份成果、confirm 是否。"""
    kind: Required[Literal["choice", "multi", "steps", "form", "compare", "confirm"]]
    question: Required[Content]
    options: Annotated[list[PromptOption], Field(max_length=12)]
    steps: Annotated[list[PromptStep], Field(max_length=8)]
    fields: Annotated[list[PromptField], Field(max_length=12)]
    allowFreeText: Required[bool]
    artifactIds: Annotated[list[Identifier], Field(min_length=2, max_length=4, description="compare 时并排展示的 2–4 份成果；让作者在一份成果里的候选（书名、人名）中挑时不用 compare，用 choice 把候选列成 options")]
    defaultOptionId: ShortText


@with_config(ConfigDict(extra="forbid", strict=True))
class StepAnswer(TypedDict, total=False):
    selectedIds: Required[Annotated[list[ShortText], Field(max_length=6)]]
    text: Content


@with_config(ConfigDict(extra="forbid", strict=True))
class AuthorAnswer(TypedDict, total=False):
    selectedIds: Required[Annotated[list[ShortText], Field(max_length=12)]]
    text: Content
    fields: dict[str, Content]
    steps: Annotated[dict[str, StepAnswer], Field(description="分步问答：问题 key → 这一问的回答")]


# ---------------------------------------------------------------------------
# 章纲批次、读者评审、定向修订
# ---------------------------------------------------------------------------
@with_config(ConfigDict(extra="forbid", strict=True))
class ChapterPlanEntry(TypedDict, total=False):
    no: Required[Order]
    title: Required[ShortText]
    outline: Required[Annotated[Content, Field(description="谁做了什么、遇到什么阻力、结果如何，80–150 字")]]
    expectation: Required[Annotated[ShortText, Field(description="本章给出的期待")]]
    payoff: Annotated[ShortText, Field(description="本章兑现的期待；没有就省略")]
    hookType: Required[HookType]
    payoffType: PayoffType
    threads: Annotated[list[Identifier], Field(max_length=10, description="本章推进或回收的线索编号")]


@with_config(ConfigDict(extra="forbid", strict=True))
class ChapterBatchPlan(TypedDict, total=False):
    """一批章纲与期待-兑现表。"""
    rulesetId: Required[Identifier]
    stageGoal: Required[ShortText]
    entries: Required[Annotated[list[ChapterPlanEntry], Field(min_length=1, max_length=20)]]


@with_config(ConfigDict(extra="forbid", strict=True))
class CriticIssue(QualityIssue, total=False):
    repairMode: Required[RepairMode]
    repairRanges: Required[Annotated[list[RepairRange], Field(max_length=10, description="每个范围为含引文段的最小完整叙述单元；只改单段或不修时返回空列表")]]
    fix: Required[Content]


@with_config(ConfigDict(extra="forbid", strict=True))
class CriticReview(TypedDict, total=False):
    """依据原文检查事实与表达；不打分。修订核验须返回 revisionAssessment。"""
    persona: ShortText
    summary: Required[Content]
    issues: Required[list[CriticIssue]]
    revisionAssessment: RevisionAssessment


@with_config(ConfigDict(extra="forbid", strict=True))
class AutomationSchedule(TypedDict, total=False):
    """自动任务的时间表：daily 给 time；weekly 给 weekday（1 周一 … 7 周日）与 time；interval 给 minutes（≥15）；once 给 at（毫秒时间戳）。"""
    type: Literal["daily", "weekly", "interval", "once"]
    time: Annotated[str, Field(pattern=r"^\d{1,2}:\d{2}$", description="本机时区的 HH:MM")]
    weekday: Annotated[int, Field(ge=1, le=7)]
    minutes: Annotated[int, Field(ge=15, le=10080)]
    at: Annotated[int, Field(ge=0)]


class SpanEdit(TypedDict, total=False):
    blockId: Annotated[str, Field(description="优先使用 targetBlocks 中宿主给出的 blockId，原样带回，禁止自编或重新计算段号")]
    index: Annotated[Order, Field(description="兼容旧任务的原始段号；使用 blockId 时由宿主确定范围")]
    endIndex: Annotated[Order, Field(description="合并连续目标段落时填写末段序号，包含末段；省略表示只改 index 所在段")]
    text: Required[Annotated[str, Field(strict=True, max_length=240000, description="该目标段或连续段块的完整替换文字。可合段；仅纯重复或分隔标记可为空，独有信息必须保留在目标块内")]]


@with_config(ConfigDict(extra="forbid", strict=True))
class SpanRevision(TypedDict, total=False):
    """定向修订：只返回被标记段落的新文本，其余段落逐字不动。"""
    paragraphs: Required[Annotated[list[SpanEdit], Field(min_length=1, max_length=60)]]


@with_config(ConfigDict(extra="forbid", strict=True))
class TaskSegment(TypedDict):
    text: Annotated[str, Field(min_length=1, description="作者原文中的连续片段，逐字保留，不改写、不增加")]
    role: Literal['writing', 'execution']


@with_config(ConfigDict(extra="forbid", strict=True))
class TaskProjection(TypedDict):
    segments: list[TaskSegment]


@with_config(ConfigDict(extra="allow", strict=True))
class CatalogEntry(TypedDict):
    id: int | str
    bookId: str
    title: str


@with_config(ConfigDict(extra="forbid", strict=True))
class CatalogCreated(TypedDict):
    created: Literal[True]
    bookId: str
    entry: CatalogEntry


@with_config(ConfigDict(extra="allow", strict=True))
class RankQueryResult(TypedDict, total=False):
    status: Required[Literal['available','partial','not_found','unavailable','cancelled','invalid_request','confirmation_required','refresh_disabled','refresh_failed']]
    error: str
    availableRankTypes: list[str]


RESULT_TYPES = {'create_chapter': CatalogCreated, 'create_volume': CatalogCreated, 'get_rank':RankQueryResult, 'get_rank_collection':RankQueryResult}


DOCUMENT_TYPES = {
    "taskProjection": TaskProjection,
    "deliveryStatus": DeliveryStatus,
    "rankSourceRequest": RankSourceRequest,
    "rankGetRequest": RankGetRequest,
    "rankCollectionRequest": RankCollectionRequest,
    "chapterCard": ChapterCard,
    "scenePlan": ScenePlan,
    "chapterBatchPlan": ChapterBatchPlan,
    "ruleset": Ruleset,
    "qualityReport": QualityReport,
    "criticReview": CriticReview,
    "spanRevision": SpanRevision,
    "platformProfile": PlatformProfile,
    "shortStoryPlan": ShortStoryPlan,
    "selectionCard": SelectionCard,
    "authorPrompt": AuthorPrompt,
    "authorAnswer": AuthorAnswer,
}


def document_schemas():
    return {name: TypeAdapter(model).json_schema() for name, model in DOCUMENT_TYPES.items()}

# Durable distillation contracts. 蒸馏的产物是文风档案：范例原文（程序按模型标出的段落取）、写法卡（每条指向一段范例）、
# 叙事模式。长度限制只约束存储与模型输出，不是文学标准。
DistillationText = Annotated[str, Field(min_length=1, max_length=4000)]


@with_config(ConfigDict(extra="forbid", strict=True))
class DistillationPick(TypedDict):
    """阅读一个片段时标出的一处有辨识度的段落：连续的段落编号，由宿主取回原文。"""
    kind: Annotated[SceneKind, Field(description="这几段属于哪种场景")]
    why: Annotated[str, Field(min_length=1, max_length=300, description="一句话说它有辨识度在哪：对白怎么接、动作与心理怎么穿插、段落怎么收")]
    sourceIds: Annotated[list[Identifier], Field(min_length=1, max_length=6, description="passages 里连续的段落编号，合计三百到六百字")]


@with_config(ConfigDict(extra="forbid", strict=True))
class DistillationLeaf(TypedDict):
    summary: Annotated[DistillationText, Field(description="这一片段在故事里做的事：怎么开篇、怎么放信息、怎么换地图；不提人名")]
    observations: Annotated[list[DistillationPick], Field(max_length=4)]


@with_config(ConfigDict(extra="forbid", strict=True))
class DistillationNote(TypedDict):
    """写法卡一条：可执行的做法，指向一到三段真实范例。"""
    text: Annotated[str, Field(min_length=1, max_length=300)]
    evidence: Annotated[list[Identifier], Field(min_length=1, max_length=3, description="输入里范例的编号")]


@with_config(ConfigDict(extra="forbid", strict=True))
class DistillationDigest(TypedDict):
    summary: Annotated[str, Field(max_length=4000, description="叙事模式：这本书怎么开篇、怎么换地图、怎么放信息，每条一行，不含人名和专有设定")]
    observations: Annotated[list[DistillationNote], Field(max_length=24)]


@with_config(ConfigDict(extra="forbid", strict=True))
class DistillationReviewItem(TypedDict):
    index: Annotated[Order, Field(description="输入 notes 里的序号")]
    verdict: Literal["keep", "revise", "omit"]
    reason: Annotated[str, Field(max_length=300)]
    text: Annotated[str, Field(max_length=300, description="核验后的写法卡；omit 留空")]


@with_config(ConfigDict(extra="forbid", strict=True))
class DistillationReview(TypedDict):
    items: Annotated[list[DistillationReviewItem], Field(max_length=40)]


DOCUMENT_TYPES.update({
    "distillationLeaf": DistillationLeaf,
    "distillationDigest": DistillationDigest,
    "distillationReview": DistillationReview,
})
