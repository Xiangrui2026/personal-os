(() => {
  const drafts = new Map();
  const hours = minutes => {
    const value = Math.round(minutes);
    if (value < 60) return value + ' 分钟';
    return Math.floor(value / 60) + ' 小时' + (value % 60 ? ' ' + value % 60 + ' 分' : '');
  };
  const quadrant = ['待补充', '重要且紧迫', '重要且从容', '紧迫', '可暂缓'];

  function union(intervals) {
    let total = 0, end = -Infinity;
    for (const [a, b] of intervals.sort((a,b) => a[0]-b[0])) {
      total += Math.max(0, b - Math.max(a, end));
      end = Math.max(end, b);
    }
    return total / 60000;
  }

  function initial(board) {
    const plan = board.plan;
    return {
      base:plan, tasks:new Set(board.tasks.filter(row => row.planned).map(row => row.task.id)),
      slots:structuredClone(plan?.layout?.slots || {}), routines:structuredClone(board.routines),
      slotInstants:Object.fromEntries(board.scheduled.filter(item => item.kind === 'plan_slot')
        .map(item => [item.task,[Date.parse(item.start), Date.parse(item.end)]])),
      budget:plan?.budget_minutes ?? null, buffer:plan?.buffer_minutes ?? 0,
      windows:structuredClone(plan?.work_windows ?? null),
    };
  }

  function render(view, data, ui) {
    const { e, action, field, input, formDialog, load, settings, edit, invoke } = ui;
    const { board, pool, priorities } = data;
    const key = view.root + '|' + board.date;
    const baseline = initial(board);
    const draft = drafts.get(key) || initial(board);
    let redrawTimer, revision=0;
    function dirty() {
      const signature = state => JSON.stringify([state.base,[...state.tasks].sort(),
        Object.entries(state.slots).sort(),Object.entries(state.routines).sort(),state.budget,state.buffer,state.windows]);
      if(signature(draft)===signature(baseline)) drafts.delete(key); else drafts.set(key,draft);
      const version=++revision;
      clearTimeout(redrawTimer);
      redrawTimer=setTimeout(async()=>{
        if(!hero.isConnected)return;
        try {
          const result=await invoke('agenda_priorities',{root:view.root,action:command(),timezone:board.timezone});
          if(!hero.isConnected || revision!==version)return;
          points.clear();
          for(const point of result) points.set(point.task, point);
          drawPool();
        } catch(error) { if(hero.isConnected && revision===version)view.notice.textContent=String(error?.message || error); }
      },220);
    }
    function command() {
      return {action:'save_agenda',date:board.date,base:draft.base,tasks:[...draft.tasks],slots:draft.slots,
        routines:draft.routines,budget_minutes:draft.budget,buffer_minutes:draft.buffer,work_windows:draft.windows};
    }
    const closed = board.plan?.closing != null;
    const byId = new Map(view.entries.map(item => [item.id,item]));
    const rows = new Map([...board.tasks,...pool].map(item => [item.task.id,item]));
    const points = new Map(priorities.map(item => [item.task,item]));
    const rules = view.entries.filter(item => item.type === 'routine');
    for (const [id, rule] of Object.entries(board.routines)) {
      if (!Object.hasOwn(draft.routines,id)) draft.routines[id] = structuredClone(rule);
    }
    const lookup = id => byId.get(id)?.title || id;
    const independent = board.scheduled.filter(item => item.kind === 'time_block');
    const reservations = new Map();
    for (const item of independent) {
      if (!reservations.has(item.task)) reservations.set(item.task, []);
      reservations.get(item.task).push([Date.parse(item.start), Date.parse(item.end)]);
    }
    const events = board.scheduled.filter(item => item.kind === 'event');
    const ranges = values => values.map(item => [Date.parse(item.start),Date.parse(item.end)]);
    const filter = { ready:true, q:'all', group:'', query:'' };
    const hero = e('header',{class:'agenda-head'});
    const dayName = new Intl.DateTimeFormat('zh-CN',{weekday:'long',timeZone:'UTC'}).format(new Date(board.date+'T12:00:00Z'));
    const date = e('input',{type:'date',value:board.date,'aria-label':'规划日期'});
    date.onchange = () => { if (date.value) { settings.date=date.value; load(view); } };
    hero.append(e('div',{},e('span',{class:'agenda-date'},board.date.replaceAll('-',' / ')+'　'+dayName),
      e('h1',{id:'page-title'},'规划今天')),e('div',{class:'agenda-tools'},date,e('a',{href:'#day',class:'pos-button'},'执行看板 ↗')));
    const capacity = e('section',{class:'agenda-capacity','aria-label':'今日时间容量'});
    const pane = title => e('section',{class:'agenda-pane','aria-label':title});
    const candidates = pane('候选任务'), planPane = pane('当日计划');
    const cardList = e('div',{class:'agenda-list'}), selectedList = e('div',{class:'agenda-list'});
    const count = e('span',{class:'agenda-count'}), selectedCount = e('span',{class:'agenda-count'});
    const query = e('input',{type:'search',placeholder:'搜索任务','aria-label':'搜索任务'});
    query.oninput = () => { filter.query=query.value.trim().toLowerCase(); drawPool(); };
    const group = e('select',{'aria-label':'按目标或项目筛选'},e('option',{value:''},'全部目标与项目'),
      ...['goal','project'].map(kind => e('optgroup',{label:kind === 'goal' ? '目标':'项目'},view.entries.filter(x=>x.type===kind)
        .map(x=>e('option',{value:x.id},x.title)))));
    group.onchange = () => { filter.group=group.value; drawPool(); };
    const ready = e('input',{type:'checkbox',checked:true});
    ready.onchange = () => { filter.ready=ready.checked; drawPool(); };
    const filters = e('div',{class:'agenda-filters','aria-label':'四象限筛选'});
    for (const [value,title] of [['all','全部'],['1','Q1 紧迫'],['2','Q2 长期'],['3','Q3'],['4','Q4'],['0','待分类']]) {
      const button=action(title,()=>{ filter.q=value; drawPool(); },'agenda-filter');
      button.dataset.q=value;
      filters.append(button);
    }
    candidates.append(e('header',{},e('h2',{},'候选任务',count),action('＋ 新建任务',()=>edit(view,'task'))),
      e('div',{class:'agenda-search'},query,group),filters,
      e('label',{class:'agenda-ready'},ready,'仅看可执行'),cardList);
    const ruleManager=()=>formDialog(view,'每日固定任务',
      [action('＋ 新建规则',()=>{ view.host.querySelector('dialog[open]')?.close(); edit(view,'routine'); }),
        ...rules.map(rule=>e('div',{class:'agenda-rule'},e('div',{},e('strong',{},rule.title),
          e('small',{},rule.from+' 起 · '+hours(rule.minutes)+(rule.until?' · 至 '+rule.until:''))),
          action('编辑规则',()=>{ view.host.querySelector('dialog[open]')?.close(); edit(view,'routine',rule); })))],
      '返回规划',async()=>{});
    planPane.append(e('header',{},e('h2',{},'当日计划',selectedCount),action('固定任务规则',ruleManager)),selectedList);
    const save = action('保存日计划',async()=>{
      save.disabled=true;
      try {
        const change=await invoke('preview',{root:view.root,reason:null,action:command()});
        ui.confirm(view,change,()=>drafts.delete(key));
      } catch (error) { view.notice.textContent=String(error?.message || error); }
      finally { save.disabled=closed || !drafts.has(key); }
    },'pos-primary');
    const reset = action('撤回草稿',()=>{ drafts.delete(key); load(view); });
    const status = e('span',{'aria-live':'polite'});
    const footer=e('div',{class:'agenda-footer'},status,e('div',{class:'agenda-tools'},reset,save));
    view.content.replaceChildren(hero,capacity,e('div',{class:'agenda-columns'},candidates,planPane),footer);

    function drawPool() {
      for(const button of filters.children) button.setAttribute('aria-pressed',String(button.dataset.q===filter.q));
      const result=pool.filter(({task,blockers})=>{
        const owner=byId.get(task.project);
        const inGroup=!filter.group || [task.project,...task.goals,...(owner?.goals || []),byId.get(task.milestone)?.goal,
          ...(owner?.milestones || []).map(id=>byId.get(id)?.goal)].includes(filter.group);
        return !draft.tasks.has(task.id) && (!filter.ready || !blockers.length) && inGroup &&
          task.title.toLowerCase().includes(filter.query) && (filter.q==='all' || filter.q===String(points.get(task.id)?.quadrant || 0));
      }).sort((a,b)=>(points.get(a.task.id)?.quadrant || 5)-(points.get(b.task.id)?.quadrant || 5) ||
        (Date.parse(points.get(a.task.id)?.deadline) || Infinity)-(Date.parse(points.get(b.task.id)?.deadline) || Infinity) || a.task.title.localeCompare(b.task.title,'zh'));
      count.textContent=String(result.length);
      cardList.replaceChildren(...result.map(({task,remaining_minutes,blockers})=>{
        const point=points.get(task.id), q=point?.quadrant || 0;
        const badge=e('span',{class:'agenda-q','data-q':q},q?'Q'+q+' '+quadrant[q]:'待分类');
        const button=action('＋ 今天',()=>{draft.tasks.add(task.id);dirty();draw();},'agenda-add');
        button.disabled=closed;
        const source={task:'截止',milestone:'阶段截止',dependency:'后续截止'}[point?.deadline_source];
        const deadline=point?.deadline ? (source || '截止')+' '+ui.timeText(point.deadline,board.timezone,true).replace('T',' ').slice(5) : '无明确期限';
        const meta=[remaining_minutes==null?'待估时':hours(remaining_minutes),deadline,
          point?.slack_minutes<0?'缺 '+hours(-point.slack_minutes):null,
          blockers.length?'等待 '+blockers.map(lookup).join('、'):null].filter(Boolean);
        return e('article',{class:'agenda-task','data-q':q},e('div',{},e('div',{class:'agenda-tags'},badge,
          e('small',{},task.project?lookup(task.project):(task.goals?.map(lookup).join('、') || '独立任务'))),
          e('h3',{},task.title),e('small',{},meta.join(' · '))),button);
      }));
      if(!result.length)cardList.append(ui.message('当前筛选下没有待选任务。'));
    }

    function setTime(id,routine) {
      const slot=routine?draft.routines[id]:draft.slots[id];
      const remaining=rows.get(id)?.remaining_minutes;
      const controls=[input('start','开始时间',slot?.start?.slice(0,5) || '',{type:'time',required:true})];
      if(!routine) controls.push(input('minutes','本次安排（分钟）',slot?.minutes ?? Math.ceil(remaining || 30),{type:'number',min:1,max:1440,required:true}));
      formDialog(view,'安排时间 · '+(routine?slot.title:lookup(id)),controls,'加入草稿',async form=>{
        const start=String(form.get('start')), minutes=routine?slot.minutes:Number(form.get('minutes'));
        const [h,m]=start.split(':').map(Number);
        if(h*60+m+minutes>1440) throw new Error('请将时间段安排在当天内。');
        if(routine) slot.start=start;
        else {
          const instant=Date.parse(await invoke('wall_time',{time:board.date+'T'+start,timezone:board.timezone}));
          draft.slots[id]={start,minutes};
          draft.slotInstants[id]=[instant,Math.min(instant+minutes*60000,Date.parse(board.window.end))];
        }
        dirty();return draw;
      });
    }
    function drawPlan() {
      const list=[];
      const append=(id,title,detail,stamp,routine,skipped=false)=>{
        const tag=stamp?stamp.slice(0,5):'待定时';
        const tools=[];
        if(!closed) {
          if(!skipped && (routine || draft.slots[id] || !reservations.has(id))) {
            tools.push(action(stamp?'改时':'定时',()=>setTime(id,routine)));
            if(stamp) tools.push(action('取消定时',()=>{
              if(routine) draft.routines[id].start=null;else delete draft.slots[id];
              dirty();draw();
            }));
          }
          tools.push(action(routine?(skipped?'恢复':'跳过'):'移回',()=>{
            if(routine)draft.routines[id].status=skipped?'todo':'skipped';
            else {draft.tasks.delete(id);delete draft.slots[id];}
            dirty();draw();
          }));
        }
        list.push({stamp, node:e('article',{class:'agenda-selected'+(skipped?' is-skipped':'')},
          e('time',{},tag),e('div',{},e('h3',{},title),e('small',{},detail)),e('div',{class:'agenda-row-tools'},tools))});
      };
      for(const [id,r] of Object.entries(draft.routines)) {
        append(id,r.title,'每日固定 · '+hours(r.minutes)+(r.status==='done'?' · 已完成':r.status==='skipped'?' · 已跳过':''),r.start,true,r.status==='skipped');
      }
      for(const id of draft.tasks) {
        const row=rows.get(id), slot=draft.slots[id];
        const old=independent.find(block=>block.task===id);
        const time=slot?.start || (old?ui.timeText(old.start,board.timezone):null);
        const minutes=slot?.minutes ?? (old?(Date.parse(old.end)-Date.parse(old.start))/60000:row?.remaining_minutes);
        append(id,lookup(id),(old && !slot?'已有时间块':'自选任务')+' · '+(minutes==null?'待估时':hours(minutes)),time,false);
      }
      for (const event of [...events,...independent.filter(item=>!draft.tasks.has(item.task) ||
        item !== independent.find(block=>block.task===item.task) || draft.slots[item.task])]) {
        const stamp=ui.timeText(event.start,board.timezone);
        list.push({stamp,node:e('article',{class:'agenda-selected agenda-event'},e('time',{},stamp),
          e('div',{},e('h3',{},event.title),e('small',{},event.kind==='event'?'固定日程':'已有独立时间块')))});
      }
      list.sort((a,b)=>(a.stamp || 'z').localeCompare(b.stamp || 'z'));
      selectedCount.textContent=String(list.length);
      selectedList.replaceChildren();
      for (const [timed,title] of [[true,'已定时'],[false,'待定时']]) {
        const items=list.filter(x=>Boolean(x.stamp)===timed);
        if(items.length) selectedList.append(e('h4',{},title),...items.map(x=>x.node));
      }
      if(!list.length) selectedList.append(ui.message('从左侧选入任务，或添加每日固定任务。'));
    }

    function drawCapacity() {
      const windows=draft.windows==null?null:ranges(draft.windows);
      const fixed=union(ranges(events).flatMap(([a,b])=>windows==null?[[a,b]]:windows.map(([lo,hi])=>
        [Math.max(a,lo),Math.min(b,hi)]).filter(([a,b])=>a<b)));
      const windowMinutes=windows==null?null:union(windows);
      const planned=new Set([...draft.tasks,...reservations.keys()]);
      let chosen=0,missing=0;
      for(const id of planned){
        const blocks=reservations.get(id) || [];
        const slot=draft.slots[id];
        const scheduled=slot && draft.slotInstants[id];
        const mins=scheduled?union([...blocks,scheduled]):blocks.length?union(blocks):slot?.minutes ?? rows.get(id)?.remaining_minutes;
        if(mins==null) missing++;else chosen+=mins;
      }
      const daily=Object.values(draft.routines).filter(r=>r.status!=='skipped').reduce((sum,r)=>sum+r.minutes,0);
      const budget=windowMinutes==null?draft.budget:Math.min(windowMinutes,draft.budget??windowMinutes);
      const used=fixed+daily+chosen+draft.buffer;
      const free=budget==null?null:budget-used;
      const segments=[['fixed','固定日程',fixed],['daily','每日固定',daily],['chosen','自选工作',chosen],
        ['buffer','缓冲',draft.buffer],['free','剩余',Math.max(0,free || 0)]];
      const bar=e('div',{class:'agenda-bar','aria-hidden':'true'});
      for(const [kind,,value] of segments) if(value>0){
        const segment=e('i',{'data-kind':kind});segment.style.flexGrow=String(value);bar.append(segment);
      }
      if(free!=null && free<0) {
        const excess=e('span',{class:'agenda-over-limit',title:'超过容量 '+hours(-free)});
        excess.style.left=(budget/used*100)+'%';
        bar.append(excess);
      }
      const windowText=(draft.windows || []).map(w=>ui.timeText(w.start,board.timezone)+'-'+
        (Date.parse(w.end)===Date.parse(board.window.end)?'24:00':ui.timeText(w.end,board.timezone))).join('\n');
      const change=action('设置容量',()=>formDialog(view,'当天时间容量',[
        field('可工作时段（每行一段）',e('textarea',{name:'windows',placeholder:'09:00-12:00\n14:00-18:00',rows:2},windowText)),
        input('budget','当天总预算（分钟）',draft.budget ?? '',{type:'number',min:0,max:1440,placeholder:'使用已配置的工作时段'}),
        input('buffer','预留缓冲（分钟）',draft.buffer,{type:'number',min:0,max:1440,required:true})],'加入草稿',async form=>{
          const raw=String(form.get('budget')), budget=raw===''?null:Number(raw), buffer=Number(form.get('buffer'));
          if(budget!=null && buffer>budget)throw new Error('缓冲超过总预算。');
          const text=String(form.get('windows')||'').trim();
          if(text!==windowText){
            const intervals=[];
            for (const line of text.split('\n').map(x=>x.trim()).filter(Boolean)){
              const match=/^((?:[01]\d|2[0-3]):[0-5]\d)\s*-\s*((?:[01]\d|2[0-3]):[0-5]\d|24:00)$/.exec(line);
              if(!match || match[1]>=match[2]) throw new Error('请按 09:00-12:00 填写有效时段。');
              const instant=time=>time==='24:00'?board.window.end:invoke('wall_time',{time:board.date+'T'+time,timezone:board.timezone});
              intervals.push({start:await instant(match[1]),end:await instant(match[2])});
            }
            intervals.sort((a,b)=>Date.parse(a.start)-Date.parse(b.start));
            if(intervals.some((w,i)=>i && Date.parse(w.start)<Date.parse(intervals[i-1].end)))throw new Error('可工作时段有重叠。');
            draft.windows=intervals.length?intervals:null;
          }
          draft.budget=budget;draft.buffer=buffer;dirty();return draw;
        }),'agenda-cap-button');
      const label=free==null?'待设置容量':free<0?'超出 '+hours(-free):'可用 '+hours(free);
      capacity.dataset.over=String(free!=null && free<0);
      capacity.replaceChildren(e('div',{class:'agenda-cap-top'},e('div',{},e('small',{},'今日时间容量'),
        e('h2',{},label)),!closed && change),bar,e('div',{class:'agenda-legend'},
        segments.map(([kind,label,value])=>e('span',{'data-kind':kind},e('i'),label,e('b',{},kind==='free' && free==null?'—':hours(value))))),
        e('small',{class:'agenda-cap-note'},(budget==null?'尚未设定总预算':'总容量 '+hours(budget))+
          (missing?' · '+missing+' 项任务待估时，以上仅含已知用时':'')));
    }
    function draw(){
      drawPool();drawPlan();drawCapacity();
      status.textContent=closed?'已结案':drafts.has(key)?'草稿 · 尚未写入':'与当前计划一致';
      save.disabled=closed || !drafts.has(key);reset.disabled=!drafts.has(key);
    }
    draw();
  }
  window.PosAgenda={render};
})();
