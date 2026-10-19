/** 思考模式：off=关闭 / default=跟随模型默认 / on=开启；请求层按供应商翻译成各家参数 */
export type AiThinkingMode = 'off' | 'default' | 'on'

/** 模型用途：text=对话/生成 image=生图 embedding=向量（全书语义记忆用，接口是 /embeddings 而非 chat） */
export type UserAiModelScene = 'text' | 'image' | 'embedding'

export interface UserAiModelSavePayload {
  id?: number
  name: string
  scene: UserAiModelScene
  provider: string
  protocol: 'openai_compatible'
  modelCode: string
  baseUrl: string
  apiKey?: string
  maxContext: number
  maxOutputTokens: number
  status: number
  sort?: number
  thinking?: AiThinkingMode
  /** 额外请求参数（JSON 对象文本），原样合并进 chat/completions 请求体，给自定义渠道填各家思考开关等 */
  extraParams?: string
}

export interface UserAiModelTestResult {
  ok: boolean
  message: string
  latency: number
  url: string
  testedAt: string
}

export interface UserAiRemoteModelListResult {
  models: string[]
  total: number
  url: string
  latency: number
  testedAt: string
}
