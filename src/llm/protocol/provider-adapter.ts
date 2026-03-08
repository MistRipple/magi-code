/**
 * Provider 协议适配器接口
 */

import {
  NormalizedModelRequest,
  NormalizedModelResponse,
  NormalizedStreamEvent,
  ProviderProtocolProfile,
} from './types';

export interface ProviderProtocolAdapter extends ProviderProtocolProfile {
  send(request: NormalizedModelRequest): Promise<NormalizedModelResponse>;
  stream(
    request: NormalizedModelRequest,
    onEvent: (event: NormalizedStreamEvent) => void,
  ): Promise<NormalizedModelResponse>;
}
