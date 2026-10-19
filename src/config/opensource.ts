// 开源版对外链接的唯一出处。仓库正式建好后只改 GITHUB_REPO_URL 这一行。
const GITHUB_REPO_URL = 'https://github.com/yilujian/easy-writing'

export const GITHUB_ISSUES_URL = `${GITHUB_REPO_URL}/issues`

export const GITHUB_NEW_ISSUE_URL = `${GITHUB_REPO_URL}/issues/new`

export const GITHUB_RELEASES_URL = `${GITHUB_REPO_URL}/releases`

/** 官网 */
export const OFFICIAL_SITE_URL = 'https://yichuang.xyz'

/** 网页端检查更新时进入官网下载安装。 */
export const OFFICIAL_DOWNLOAD_URL = `${OFFICIAL_SITE_URL}/#download`

/** 反馈页的联系方式：不会用 GitHub 的作者走这里 */
export const CONTACT_EMAIL = '75082807@qq.com'

/**
 * 微信二维码先取官网上的最新一张，取不到再用安装包里带的那张（docs/images/wechat-contact.png 与反馈群码）。
 * 群二维码只有 7 天有效期，换新码时只需把新图传到官网这个路径，不用发版。
 */
export const WECHAT_GROUP_QR_URL = `${OFFICIAL_SITE_URL}/contact/wechat-group.png`
export const WECHAT_PERSONAL_QR_URL = `${OFFICIAL_SITE_URL}/contact/wechat.png`
