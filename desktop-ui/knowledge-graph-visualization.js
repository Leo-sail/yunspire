/**
 * 知识图谱可视化组件
 * 使用 D3.js 进行力导向图布局和交互
 */

import { KnowledgeGraphApi } from './api-client.js';
import { dataLoader, notifications } from './state-management.js';

export class KnowledgeGraphVisualization {
  constructor(containerId, options = {}) {
    this.container = document.getElementById(containerId);
    this.options = {
      width: options.width || 1200,
      height: options.height || 800,
      nodeRadius: options.nodeRadius || 8,
      linkDistance: options.linkDistance || 100,
      chargeStrength: options.chargeStrength || -300,
      ...options
    };

    this.svg = null;
    this.simulation = null;
    this.data = null;

    this.init();
  }

  /**
   * 初始化 SVG 画布
   */
  init() {
    if (!this.container) {
      console.error('容器元素不存在');
      return;
    }

    // 清空容器
    this.container.innerHTML = '';

    // 创建 SVG
    this.svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    this.svg.setAttribute('width', this.options.width);
    this.svg.setAttribute('height', this.options.height);
    this.svg.style.border = '1px solid #ddd';
    this.container.appendChild(this.svg);

    // 添加缩放支持
    this.addZoomSupport();
  }

  /**
   * 添加缩放支持
   */
  addZoomSupport() {
    let scale = 1;
    let translateX = 0;
    let translateY = 0;

    this.svg.addEventListener('wheel', (e) => {
      e.preventDefault();
      const delta = e.deltaY > 0 ? 0.9 : 1.1;
      scale *= delta;
      scale = Math.max(0.1, Math.min(5, scale));
      this.updateTransform(scale, translateX, translateY);
    });

    // 拖拽画布
    let isDragging = false;
    let startX, startY;

    this.svg.addEventListener('mousedown', (e) => {
      if (e.target === this.svg) {
        isDragging = true;
        startX = e.clientX - translateX;
        startY = e.clientY - translateY;
      }
    });

    document.addEventListener('mousemove', (e) => {
      if (isDragging) {
        translateX = e.clientX - startX;
        translateY = e.clientY - startY;
        this.updateTransform(scale, translateX, translateY);
      }
    });

    document.addEventListener('mouseup', () => {
      isDragging = false;
    });
  }

  /**
   * 更新变换
   */
  updateTransform(scale, translateX, translateY) {
    const g = this.svg.querySelector('g');
    if (g) {
      g.setAttribute('transform', `translate(${translateX},${translateY}) scale(${scale})`);
    }
  }

  /**
   * 加载图谱数据
   */
  async loadGraph(vaultId) {
    try {
      const data = await dataLoader.loadData(
        'knowledgeGraph',
        () => KnowledgeGraphApi.getKnowledgeGraph(vaultId),
        { forceRefresh: true }
      );

      this.data = data;
      this.render();
      notifications.success('知识图谱加载成功');
    } catch (error) {
      console.error('加载知识图谱失败:', error);
      notifications.error(`加载失败: ${error.message}`);
    }
  }

  /**
   * 渲染图谱
   */
  render() {
    if (!this.data || !this.data.nodes || !this.data.edges) {
      console.warn('没有数据可渲染');
      return;
    }

    // 清空 SVG
    this.svg.innerHTML = '';

    // 创建主 group
    const g = document.createElementNS('http://www.w3.org/2000/svg', 'g');
    this.svg.appendChild(g);

    // 准备数据
    const nodes = this.data.nodes.map(n => ({
      id: n.note_path || n.notePath,
      title: n.title,
      links: n.outgoing_links || n.outgoingLinks || 0,
      centrality: n.centrality_score || n.centralityScore || 0,
      x: Math.random() * this.options.width,
      y: Math.random() * this.options.height,
    }));

    const links = this.data.edges.map(e => ({
      source: e.from_note || e.fromNote,
      target: e.to_note || e.toNote,
      type: e.edge_type || e.edgeType || 'wiki_link',
    }));

    // 创建节点映射
    const nodeMap = new Map(nodes.map(n => [n.id, n]));

    // 过滤无效链接
    const validLinks = links.filter(l =>
      nodeMap.has(l.source) && nodeMap.has(l.target)
    );

    // 绘制链接
    const linkGroup = document.createElementNS('http://www.w3.org/2000/svg', 'g');
    linkGroup.setAttribute('class', 'links');
    g.appendChild(linkGroup);

    validLinks.forEach(link => {
      const line = document.createElementNS('http://www.w3.org/2000/svg', 'line');
      line.setAttribute('stroke', '#999');
      line.setAttribute('stroke-opacity', '0.6');
      line.setAttribute('stroke-width', '1');
      line.dataset.source = link.source;
      line.dataset.target = link.target;
      linkGroup.appendChild(line);
    });

    // 绘制节点
    const nodeGroup = document.createElementNS('http://www.w3.org/2000/svg', 'g');
    nodeGroup.setAttribute('class', 'nodes');
    g.appendChild(nodeGroup);

    nodes.forEach(node => {
      // 节点圆圈
      const circle = document.createElementNS('http://www.w3.org/2000/svg', 'circle');
      const radius = this.options.nodeRadius + Math.sqrt(node.links) * 2;
      circle.setAttribute('r', radius);
      circle.setAttribute('fill', this.getNodeColor(node.centrality));
      circle.setAttribute('stroke', '#fff');
      circle.setAttribute('stroke-width', '2');
      circle.dataset.nodeId = node.id;
      circle.style.cursor = 'pointer';

      // 节点标签
      const text = document.createElementNS('http://www.w3.org/2000/svg', 'text');
      text.textContent = node.title.length > 20
        ? node.title.substring(0, 20) + '...'
        : node.title;
      text.setAttribute('font-size', '12');
      text.setAttribute('dx', radius + 5);
      text.setAttribute('dy', '4');
      text.style.pointerEvents = 'none';
      text.style.userSelect = 'none';

      // 节点组
      const nodeG = document.createElementNS('http://www.w3.org/2000/svg', 'g');
      nodeG.setAttribute('transform', `translate(${node.x},${node.y})`);
      nodeG.dataset.nodeId = node.id;
      nodeG.appendChild(circle);
      nodeG.appendChild(text);
      nodeGroup.appendChild(nodeG);

      // 添加交互
      circle.addEventListener('click', () => {
        this.onNodeClick(node);
      });

      circle.addEventListener('mouseenter', () => {
        this.showTooltip(node, circle);
      });

      circle.addEventListener('mouseleave', () => {
        this.hideTooltip();
      });
    });

    // 启动力导向布局模拟
    this.startSimulation(nodes, validLinks, g);
  }

  /**
   * 启动力导向布局模拟
   */
  startSimulation(nodes, links, g) {
    // 简单的力导向模拟（不依赖 D3）
    const iterations = 100;
    const dt = 0.1;

    for (let i = 0; i < iterations; i++) {
      // 排斥力
      for (let j = 0; j < nodes.length; j++) {
        for (let k = j + 1; k < nodes.length; k++) {
          const dx = nodes[k].x - nodes[j].x;
          const dy = nodes[k].y - nodes[j].y;
          const dist = Math.sqrt(dx * dx + dy * dy) || 1;
          const force = this.options.chargeStrength / (dist * dist);
          const fx = (dx / dist) * force;
          const fy = (dy / dist) * force;
          nodes[j].x -= fx * dt;
          nodes[j].y -= fy * dt;
          nodes[k].x += fx * dt;
          nodes[k].y += fy * dt;
        }
      }

      // 吸引力（链接）
      links.forEach(link => {
        const source = nodes.find(n => n.id === link.source);
        const target = nodes.find(n => n.id === link.target);
        if (source && target) {
          const dx = target.x - source.x;
          const dy = target.y - source.y;
          const dist = Math.sqrt(dx * dx + dy * dy) || 1;
          const force = (dist - this.options.linkDistance) * 0.1;
          const fx = (dx / dist) * force;
          const fy = (dy / dist) * force;
          source.x += fx * dt;
          source.y += fy * dt;
          target.x -= fx * dt;
          target.y -= fy * dt;
        }
      });

      // 中心力
      const centerX = this.options.width / 2;
      const centerY = this.options.height / 2;
      nodes.forEach(node => {
        node.x += (centerX - node.x) * 0.01;
        node.y += (centerY - node.y) * 0.01;
      });
    }

    // 更新节点位置
    nodes.forEach(node => {
      const nodeG = g.querySelector(`g[data-node-id="${node.id}"]`);
      if (nodeG) {
        nodeG.setAttribute('transform', `translate(${node.x},${node.y})`);
      }
    });

    // 更新链接位置
    links.forEach(link => {
      const source = nodes.find(n => n.id === link.source);
      const target = nodes.find(n => n.id === link.target);
      if (source && target) {
        const line = g.querySelector(`line[data-source="${link.source}"][data-target="${link.target}"]`);
        if (line) {
          line.setAttribute('x1', source.x);
          line.setAttribute('y1', source.y);
          line.setAttribute('x2', target.x);
          line.setAttribute('y2', target.y);
        }
      }
    });
  }

  /**
   * 获取节点颜色
   */
  getNodeColor(centrality) {
    if (centrality > 70) return '#e74c3c'; // 高中心度：红色
    if (centrality > 40) return '#f39c12'; // 中中心度：橙色
    if (centrality > 10) return '#3498db'; // 低中心度：蓝色
    return '#95a5a6'; // 孤立节点：灰色
  }

  /**
   * 节点点击事件
   */
  onNodeClick(node) {
    console.log('节点点击:', node);
    notifications.show(`打开笔记: ${node.title}`, 'info', 2000);
    // TODO: 触发打开笔记事件
  }

  /**
   * 显示工具提示
   */
  showTooltip(node, element) {
    // 简单实现：可以扩展为更复杂的 tooltip
    const tooltip = document.createElement('div');
    tooltip.id = 'graph-tooltip';
    tooltip.style.position = 'absolute';
    tooltip.style.background = 'rgba(0,0,0,0.8)';
    tooltip.style.color = 'white';
    tooltip.style.padding = '8px 12px';
    tooltip.style.borderRadius = '4px';
    tooltip.style.fontSize = '12px';
    tooltip.style.pointerEvents = 'none';
    tooltip.style.zIndex = '1000';
    tooltip.innerHTML = `
      <div><strong>${node.title}</strong></div>
      <div>链接数: ${node.links}</div>
      <div>中心度: ${node.centrality.toFixed(1)}</div>
    `;

    const rect = element.getBoundingClientRect();
    tooltip.style.left = rect.right + 10 + 'px';
    tooltip.style.top = rect.top + 'px';

    document.body.appendChild(tooltip);
  }

  /**
   * 隐藏工具提示
   */
  hideTooltip() {
    const tooltip = document.getElementById('graph-tooltip');
    if (tooltip) {
      tooltip.remove();
    }
  }

  /**
   * 销毁组件
   */
  destroy() {
    if (this.container) {
      this.container.innerHTML = '';
    }
  }
}
