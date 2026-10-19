import { api } from '../api'

/* 直播分数监控的接口层（2026-10-20）
   后端是 Rust pk-server 的 /api/live/*，字段 camelCase。 */

export interface LiveRules {
  firstWaveGroupCount: number
  groupRoundsPerGroup: number
  directQualifyPerGroup: number
  revivalRoomCapacity: number
  revivalQualifyPerRoom: number
  promotionGroupCount: number
  promotionGroupSize: number
  promotionQualifyPerGroup: number
  finalRoomCapacity: number
  settleGraceSecs: number
}

export interface LiveEvent {
  id: number
  title: string
  eventDate: string
  status: string
  dayPlanId: number
  nightPlanId: number
  ruleVersion: number
  rules: LiveRules
  pushEnabled: boolean
  pushIntervalSecs: number
  updatedAt: string
}

export interface LiveStage {
  id: number
  kind: string
  seq: number
  status: string
  startedAt: string | null
  finishedAt: string | null
}

export interface LiveGroup {
  id: number
  stageId: number
  stageKind: string
  stageStatus: string
  kind: string
  seq: number
  name: string
  status: string
  qualifySlots: number
  sourceGroupId: number
  officialRoundId: number
  finalizedAt: string | null
}

export interface MonitorRoom {
  id: number
  webRid: string
  roomId: string
  url: string
  label: string
  enabled: boolean
  health: string
  lastSeenAt: string | null
  lastError: string
}

export interface LiveQualifier {
  stageId: number
  stageKind: string
  groupId: number
  roundId: number
  personId: number
  personName: string
  douyinUid: string
  source: string
  rank: number
  score: number
}

export interface Overview {
  event: LiveEvent | null
  stages: LiveStage[]
  groups: LiveGroup[]
  rooms: MonitorRoom[]
  qualifiers: LiveQualifier[]
}

export interface LiveRound {
  id: number
  battleId: string
  roundKind: string
  attemptNo: number
  parentRoundId: number
  status: string
  isOfficial: boolean
  endReason: string
  rosterSignature: string
  startAt: string | null
  actualEndAt: string | null
}

export interface LiveSeat {
  personId: number
  douyinUid: string
  nickname: string
  teamId: string
  score: number
  rank: number
  qualified: boolean
}

export interface LiveScores {
  eventId: number
  groupId: number
  roundId: number
  scores: Array<{
    douyinUid: string
    score: number
    rank: number
    teamScore: number
    teamRank: number
    observedAt: string
  }>
  seats: LiveSeat[]
}

export const live = {
  events: () => api<{ items: LiveEvent[] }>('/api/live/events'),

  createEvent: (body: {
    title: string
    eventDate: string
    dayPlanId?: number
    rules?: Partial<LiveRules>
  }) => api<{ id: number }>('/api/live/events', { method: 'POST', body: JSON.stringify(body) }),

  updateRules: (id: number, rules: Partial<LiveRules>, reason?: string) =>
    api<{ ok: boolean }>(`/api/live/events/${id}/rules`, {
      method: 'PUT',
      body: JSON.stringify({ rules, reason }),
    }),

  /** 推送开关与间隔（赛事层第二层开关，第一层是 PUSH_OUT_ENABLED） */
  updatePush: (id: number, pushEnabled: boolean, pushIntervalSecs: number) =>
    api<{ ok: boolean; pushEnabled: boolean; pushIntervalSecs: number }>(
      `/api/live/events/${id}/push`,
      { method: 'PUT', body: JSON.stringify({ pushEnabled, pushIntervalSecs }) },
    ),

  addMonitor: (id: number, url: string, label?: string) =>
    api<{ id: number; webRid: string }>(`/api/live/events/${id}/monitors`, {
      method: 'POST',
      body: JSON.stringify({ url, label }),
    }),

  overview: (id: number) => api<Overview>(`/api/live/events/${id}/overview`),

  scores: (id: number, groupId: number) =>
    api<LiveScores>(`/api/live/events/${id}/scores?groupId=${groupId}`),

  bindPlans: (id: number, dayPlanId?: number, nightPlanId?: number) =>
    api<{ ok: boolean }>(`/api/live/events/${id}/plans`, {
      method: 'POST',
      body: JSON.stringify({ dayPlanId, nightPlanId }),
    }),

  buildDayGroups: (id: number) =>
    api<{ firstWave: number; rest: number }>(`/api/live/events/${id}/day-groups`, { method: 'POST' }),

  buildRevival: (id: number, wave: 'first' | 'rest') =>
    api<{ rooms: number }>(`/api/live/events/${id}/revival?wave=${wave}`, { method: 'POST' }),

  buildPromotion: (id: number) =>
    api<{ groups: number }>(`/api/live/events/${id}/promotion`, { method: 'POST' }),

  groupRounds: (groupId: number) => api<{ items: LiveRound[] }>(`/api/live/groups/${groupId}/rounds`),

  /** 选正式局（同组两场完整对局时用） */
  chooseOfficial: (groupId: number, roundId: number) =>
    api<{ ok: boolean; roundId: number }>(`/api/live/groups/${groupId}/official`, {
      method: 'POST',
      body: JSON.stringify({ roundId }),
    }),

  voidRound: (roundId: number) =>
    api<{ ok: boolean }>(`/api/live/rounds/${roundId}/void`, { method: 'POST' }),

  finalizeGroup: (groupId: number) =>
    api<{ ok: boolean; roundId: number }>(`/api/live/groups/${groupId}/finalize`, { method: 'POST' }),

  freezeGroup: (groupId: number, source: 'direct' | 'revival' | 'promotion') =>
    api<{ ok: boolean; qualified: number }>(`/api/live/groups/${groupId}/freeze`, {
      method: 'POST',
      body: JSON.stringify({ source }),
    }),

  freezeStage: (eventId: number, wave: 'first' | 'rest', source: 'direct' | 'revival' | 'promotion') =>
    api<{ ok: boolean; groups: number; qualified: number }>(
      `/api/live/stages/freeze?eventId=${eventId}&wave=${wave}`,
      { method: 'POST', body: JSON.stringify({ source }) },
    ),

  /** 给这一局的主播推一次当前分数 */
  pushRoundScores: (roundId: number) =>
    api<{ ok: boolean; sent: number }>(`/api/live/rounds/${roundId}/push`, { method: 'POST' }),
}
