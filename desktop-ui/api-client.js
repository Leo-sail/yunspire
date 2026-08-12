/**
 * 云枢 API 客户端
 * 统一的后端 API 调用封装，支持类型安全和错误处理
 */

import { invoke } from '@tauri-apps/api/core';

/**
 * API 响应格式
 */
export interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: ApiError;
  trace_id?: string;
  timestamp: string;
}

/**
 * API 错误
 */
export interface ApiError {
  code: string;
  message: string;
  details?: string;
  retryable: boolean;
}

/**
 * 请求选项
 */
export interface RequestOptions {
  timeout?: number;
  retryOnError?: boolean;
  maxRetries?: number;
  onProgress?: (progress: number) => void;
}

/**
 * API 客户端类
 */
export class ApiClient {
  private defaultTimeout = 30000; // 30 秒
  private defaultMaxRetries = 3;

  /**
   * 调用 Tauri 命令
   */
  async invoke<T>(
    command: string,
    args?: Record<string, unknown>,
    options?: RequestOptions
  ): Promise<T> {
    const timeout = options?.timeout || this.defaultTimeout;
    const maxRetries = options?.maxRetries || this.defaultMaxRetries;
    let lastError: Error | null = null;

    for (let attempt = 0; attempt < maxRetries; attempt++) {
      try {
        // 设置超时
        const timeoutPromise = new Promise<never>((_, reject) => {
          setTimeout(() => reject(new Error('请求超时')), timeout);
        });

        // 调用 Tauri 命令
        const invokePromise = invoke<T>(command, args);

        // 竞争：先完成的获胜
        const result = await Promise.race([invokePromise, timeoutPromise]);
        return result;
      } catch (error) {
        lastError = error as Error;

        // 如果不需要重试或不是最后一次尝试，继续
        if (!options?.retryOnError || attempt === maxRetries - 1) {
          break;
        }

        // 等待一段时间后重试
        await new Promise(resolve => setTimeout(resolve, 1000 * (attempt + 1)));
      }
    }

    throw lastError || new Error('未知错误');
  }

  /**
   * 调用返回统一 ApiResponse 格式的命令
   */
  async invokeApi<T>(
    command: string,
    args?: Record<string, unknown>,
    options?: RequestOptions
  ): Promise<T> {
    const response = await this.invoke<ApiResponse<T>>(command, args, options);

    if (!response.success) {
      throw new ApiClientError(
        response.error?.message || '请求失败',
        response.error?.code || 'UNKNOWN_ERROR',
        response.error?.retryable || false
      );
    }

    if (!response.data) {
      throw new ApiClientError('响应数据为空', 'EMPTY_RESPONSE', false);
    }

    return response.data;
  }
}

/**
 * API 客户端错误
 */
export class ApiClientError extends Error {
  constructor(
    message: string,
    public code: string,
    public retryable: boolean
  ) {
    super(message);
    this.name = 'ApiClientError';
  }
}

/**
 * 全局 API 客户端实例
 */
export const apiClient = new ApiClient();

/**
 * 知识图谱相关 API
 */
export class KnowledgeGraphApi {
  /**
   * 获取知识图谱
   */
  static async getKnowledgeGraph(vaultId: string) {
    return apiClient.invoke('build_knowledge_graph', { vaultId });
  }

  /**
   * 获取中心节点
   */
  static async getHubNodes(vaultId: string, limit?: number) {
    return apiClient.invoke('get_hub_nodes', { vaultId, limit });
  }

  /**
   * 获取孤立节点
   */
  static async getIsolatedNodes(vaultId: string) {
    return apiClient.invoke('get_isolated_nodes', { vaultId });
  }

  /**
   * 查找相关笔记
   */
  static async findRelatedNotes(vaultId: string, notePath: string, limit?: number) {
    return apiClient.invoke('find_related_notes', { vaultId, notePath, limit });
  }

  /**
   * 获取图谱可视化数据
   */
  static async getGraphVisualization(vaultId: string, layoutType?: string) {
    return apiClient.invoke('get_graph_visualization', { vaultId, layoutType });
  }

  /**
   * 获取节点子图
   */
  static async getNodeSubgraph(vaultId: string, notePath: string, depth?: number) {
    return apiClient.invoke('get_node_subgraph', { vaultId, notePath, depth });
  }
}

/**
 * 内容价值相关 API
 */
export class ContentValueApi {
  /**
   * 计算笔记价值
   */
  static async calculateNoteValue(vaultId: string, notePath: string) {
    return apiClient.invoke('calculate_note_value', { vaultId, notePath });
  }

  /**
   * 获取价值排行榜
   */
  static async getValueRankedNotes(vaultId: string, limit?: number) {
    return apiClient.invoke('get_value_ranked_notes', { vaultId, limit });
  }

  /**
   * 获取价值报告
   */
  static async getValueReport(vaultId: string) {
    return apiClient.invoke('get_value_report', { vaultId });
  }

  /**
   * 批量计算笔记价值
   */
  static async batchCalculateValue(vaultId: string, notePaths: string[]) {
    return apiClient.invoke('calculate_notes_value_batch', { vaultId, notePaths });
  }

  /**
   * 获取可操作建议
   */
  static async getActionableSuggestions(vaultId: string, limit?: number) {
    return apiClient.invoke('get_actionable_suggestions', { vaultId, limit });
  }
}

/**
 * 知识健康相关 API
 */
export class KnowledgeHealthApi {
  /**
   * 获取知识库健康度仪表盘
   */
  static async getHealthDashboard(vaultId: string) {
    return apiClient.invoke('get_knowledge_health_dashboard', { vaultId });
  }
}

/**
 * 性能监控相关 API
 */
export class PerformanceApi {
  /**
   * 获取性能报告
   */
  static async getPerformanceReport() {
    return apiClient.invoke('get_performance_report');
  }

  /**
   * 清空性能指标
   */
  static async clearPerformanceMetrics() {
    return apiClient.invoke('clear_performance_metrics');
  }

  /**
   * 获取内存统计
   */
  static async getMemoryStats() {
    return apiClient.invoke('get_memory_stats');
  }
}

/**
 * 间隔重复学习相关 API
 */
export class SpacedRepetitionApi {
  /**
   * 记录笔记复习
   */
  static async recordNoteReview(vaultId: string, notePath: string, quality: number) {
    return apiClient.invoke('record_note_review', { vaultId, notePath, quality });
  }

  /**
   * 获取待复习笔记
   */
  static async getDueForReview(vaultId: string, now: string, limit?: number) {
    return apiClient.invoke('get_due_for_review', { vaultId, now, limit });
  }

  /**
   * 获取复习计划摘要
   */
  static async getReviewPlanSummary(vaultId: string, now: string) {
    return apiClient.invoke('get_review_plan_summary', { vaultId, now });
  }
}

/**
 * AI 内容智能相关 API
 */
export class ContentIntelligenceApi {
  /**
   * 生成笔记摘要
   */
  static async generateNoteSummary(vaultId: string, notePath: string, maxSentences?: number) {
    return apiClient.invoke('generate_note_summary', { vaultId, notePath, maxSentences });
  }

  /**
   * 提取关键词
   */
  static async extractKeywords(vaultId: string, notePath: string, maxKeywords?: number) {
    return apiClient.invoke('extract_keywords', { vaultId, notePath, maxKeywords });
  }

  /**
   * 识别笔记主题
   */
  static async identifyNoteTopic(vaultId: string, notePath: string) {
    return apiClient.invoke('identify_note_topic', { vaultId, notePath });
  }

  /**
   * 推荐相似内容
   */
  static async recommendSimilarContent(vaultId: string, notePath: string, maxRecommendations?: number) {
    return apiClient.invoke('recommend_similar_content', { vaultId, notePath, maxRecommendations });
  }
}

/**
 * 使用指标相关 API
 */
export class MetricsApi {
  /**
   * 获取指标报告
   */
  static async getMetricsReport() {
    return apiClient.invoke('get_metrics_report');
  }
}
