import type { Player } from '@pk/shared';

/** 一个分组桶：id 稳定以便 dnd-kit 定位容器，name 可编辑 */
export interface Bucket {
  id: string;
  name: string;
  players: Player[];
}
