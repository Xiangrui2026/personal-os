const groups = [
  { label: '工作区', pages: ['day', 'overview', 'roadmap', 'projects', 'knowledge', 'resources', 'review'] },
  { label: '数据管理', pages: ['changes', 'backup'] },
];

const pages = {
  day: { label: '每日看板', icon: '\uE787', description: '安排任务、日程和专注时间' },
  overview: { label: '总览', icon: '\uE80F' },
  roadmap: { label: '目标路线图', icon: '\uE902', description: '长期目标与阶段里程碑' },
  projects: { label: '项目', icon: '\uE8F1', description: '计划、任务与研究进展' },
  knowledge: { label: '知识库', icon: '\uE82D', description: 'Obsidian 笔记与双链' },
  resources: { label: '资源', icon: '\uE8B7', description: '文献、书籍与手写资料' },
  review: { label: '手稿审核', icon: '\uE70B', description: '手稿预览与审核意见' },
  changes: { label: '变更历史', icon: '\uE81C', description: '语义变更记录与差异' },
  backup: { label: '备份与恢复', icon: '\uE72C', description: '恢复点与备份记录' },
};

const pageContent = {
  knowledge: () => `
    <div class="detail-grid">
      ${panel('笔记', '暂无笔记', 'knowledge')}
      ${panel('笔记预览', '暂无预览', 'knowledge')}
    </div>`,
  resources: () => `
    <section aria-labelledby="sources-heading">
      <h2 class="section-heading" id="sources-heading">资料来源</h2>
      <div class="source-grid">
        ${source('Zotero', '论文与引用', '\uE8A5')}
        ${source('iCloud', '书籍、教材与试卷', '\uE753')}
        ${source('Noteful', '手稿与阅读批注', '\uE70B')}
      </div>
    </section>
    ${table('资源列表', ['名称', '来源', '类型'], '暂无资源')}`,
  review: () => `
    <div class="detail-grid">
      ${panel('待审核手稿', '暂无手稿', 'review')}
      ${panel('审核工作台', '暂无审核内容', 'review')}
    </div>`,
  changes: () => table('Change 记录', ['时间', '变更类型', '对象', '操作者'], '暂无变更记录'),
  backup: () => `
    <div class="detail-grid">
      ${panel('恢复点', '暂无恢复点', 'backup')}
      ${panel('备份记录', '暂无备份记录', 'backup')}
    </div>`,
};

const navigation = document.querySelector('#navigation');
const breadcrumb = document.querySelector('#breadcrumb');
const page = document.querySelector('#page');

function glyph(character) {
  return `<span class="glyph" aria-hidden="true">${character}</span>`;
}

function tile(id) {
  return `<span class="tile-icon ${id}">${glyph(pages[id].icon)}</span>`;
}

function panel(title, message, id) {
  return `<section class="panel">
    <h2>${title}</h2>
    <div class="empty">${tile(id)}<p>${message}</p></div>
  </section>`;
}

function table(title, headings, message) {
  return `<section class="panel table-panel">
    <h2>${title}</h2>
    <table>
      <thead><tr>${headings.map(label => `<th scope="col">${label}</th>`).join('')}</tr></thead>
      <tbody><tr><td colspan="${headings.length}">${message}</td></tr></tbody>
    </table>
  </section>`;
}

function source(name, description, character) {
  return `<article class="source">
    <span class="tile-icon resources">${glyph(character)}</span>
    <div><h3>${name}</h3><p>${description}</p></div>
  </article>`;
}

function card(id) {
  const { label, description } = pages[id];
  return `<a class="card" href="#${id}">
    <div class="card-top">${tile(id)}<span class="arrow" aria-hidden="true">↗</span></div>
    <h3>${label}</h3>
    <p>${description}</p>
  </a>`;
}

function overview() {
  const workspace = groups[0].pages.filter(id => id !== 'overview');
  return `
    <section class="hero" aria-labelledby="page-title">
      <div class="hero-art" aria-hidden="true">
        <span class="orbit outer"></span><span class="orbit middle"></span><span class="orbit inner"></span>
        <span class="point one"></span><span class="point two"></span><span class="point three"></span>
        <span class="hero-mark"><i></i><i></i><i></i><i></i></span>
      </div>
      <div class="hero-content">
        <h1 id="page-title">个人工作台</h1>
        <p>在这里查看目标、项目与知识资料。</p>
        <a class="primary-link" href="#roadmap">查看目标路线图 <span aria-hidden="true">→</span></a>
      </div>
    </section>
    <section class="overview-section" aria-labelledby="workspace-heading">
      <h2 class="section-heading" id="workspace-heading">工作区</h2>
      <div class="card-grid">${workspace.map(card).join('')}</div>
    </section>
    <section class="overview-section" aria-labelledby="data-heading">
      <h2 class="section-heading" id="data-heading">数据管理</h2>
      <div class="card-grid data-grid">${groups[1].pages.map(card).join('')}</div>
    </section>`;
}

navigation.innerHTML = groups.map(({ label, pages: ids }) => `
  <div class="nav-group">
    <div class="nav-caption">${label}</div>
    ${ids.map(id => `<a class="nav-link" href="#${id}">${glyph(pages[id].icon)}<span>${pages[id].label}</span></a>`).join('')}
  </div>`).join('');

function render({ focus = false } = {}) {
  const hash = window.location.hash.slice(1);
  const id = Object.hasOwn(pages, hash) ? hash : 'day';
  const { label, description } = pages[id];

  for (const link of navigation.querySelectorAll('a')) {
    if (link.hash === `#${id}`) link.setAttribute('aria-current', 'page');
    else link.removeAttribute('aria-current');
  }

  breadcrumb.textContent = label;
  page.innerHTML = id === 'overview' ? overview() : `
    <header class="page-heading">
      ${tile(id)}
      <h1 id="page-title">${label}</h1>
      <p>${description}</p>
    </header>
    ${['day', 'roadmap', 'projects'].includes(id) ? '<div id="productivity"></div>' : pageContent[id]()}`;
  const productivity = page.querySelector('#productivity');
  if (productivity) window.PosProductivity.mount(productivity, id);
  page.parentElement.scrollTop = 0;
  if (focus) page.focus({ preventScroll: true });
}

window.addEventListener('hashchange', () => render({ focus: true }));
render();
