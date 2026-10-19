import type { AgentSkillDraft } from '@/types/agent-creation'

export const blankSkillDraft = (): AgentSkillDraft => ({
  id: '',
  name: '',
  description: '',
  instructions: '',
  files: []
})
