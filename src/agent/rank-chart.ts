import { echarts, type EChartsOption } from '@/utils/echarts'
import type { NovelRankTagTrendResult } from '@/types/novel-rank'

/**
 * 榜单趋势图：把本机榜单快照的数据画成 PNG，发到聊天通道。
 * 聊天里没有页面，图要自带标题与图例；在一个挂到屏幕外的容器里用 echarts 渲染后导出。
 */

export const CHART_WIDTH = 960
export const CHART_HEIGHT = 560

/** 标签趋势折线：每个标签一条线，横轴是统计日期 */
export const buildTagTrendChartOption = (data: NovelRankTagTrendResult, title: string): EChartsOption => {
  const dates = [...new Set(data.list.flatMap(item => item.series.map(point => point.date)))].sort()
  return {
    animation: false,
    backgroundColor: '#ffffff',
    title: { text: title, left: 'center', top: 14, textStyle: { fontSize: 20, fontWeight: 600, color: '#241F1B' } },
    legend: { type: 'scroll', bottom: 12, textStyle: { fontSize: 13, color: '#4A443F' } },
    grid: { left: 56, right: 28, top: 72, bottom: 72 },
    xAxis: { type: 'category', data: dates, axisLabel: { fontSize: 12, color: '#4A443F' } },
    yAxis: { type: 'value', name: '上榜作品数', minInterval: 1, axisLabel: { fontSize: 12, color: '#4A443F' } },
    series: data.list.map(item => ({
      name: item.tag,
      type: 'line',
      smooth: true,
      symbolSize: 7,
      lineStyle: { width: 2.5 },
      data: dates.map(date => item.series.find(point => point.date === date)?.count ?? 0),
    })),
  }
}

/** 离屏渲染并导出 PNG；返回不带 data: 前缀的 base64 */
export const renderChartPng = async (option: EChartsOption): Promise<string> => {
  if (typeof document === 'undefined') throw new Error('当前环境不能绘图')
  const host = document.createElement('div')
  host.style.cssText = `position:fixed;left:-99999px;top:0;width:${CHART_WIDTH}px;height:${CHART_HEIGHT}px;`
  document.body.appendChild(host)
  const chart = echarts.init(host, undefined, { renderer: 'canvas', width: CHART_WIDTH, height: CHART_HEIGHT, devicePixelRatio: 2 })
  try {
    chart.setOption(option)
    const dataUrl = chart.getDataURL({ type: 'png', pixelRatio: 2, backgroundColor: '#ffffff' })
    const base64 = dataUrl.replace(/^data:image\/png;base64,/, '')
    if (!base64 || base64 === dataUrl) throw new Error('图表导出失败')
    return base64
  } finally {
    chart.dispose()
    host.remove()
  }
}
