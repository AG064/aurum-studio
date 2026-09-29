(function () {
  'use strict';
  const ready = () => {
    const $ = id => document.getElementById(id);
    let sceneHash = null, sceneFile = '', fileHash = null, loadedFile = '';
    let selected = null, watching = false, dialogAction = 'create', agent = null;
    let activeRoot='', godotFolder='godot', dirty=false, draftTimer=null, draftStored=true, pendingDraft=null;
    const draftId=crypto.randomUUID();
    const headers = () => {
      const value = { 'Content-Type': 'application/json' };
      const token = sessionStorage.getItem('aurum_token');
      if (token) value['X-Aurum-Token'] = token;
      return value;
    };
    async function api(path, body) {
      const result = await fetch(path, { method: body ? 'POST' : 'GET', headers: headers(), ...(body ? { body: JSON.stringify(body) } : {}) });
      const text = await result.text();
      let value;
      try { value = JSON.parse(text); } catch (_) { throw new Error(text || 'The server returned no result'); }
      if (!result.ok || value.ok === false) {
        const error = new Error(value.error || value.report_error || (value.report?.failures || value.errors || value.log || []).join('\n') || 'Operation failed');
        error.result = value;
        throw error;
      }
      return value;
    }
    async function task(label, action) {
      $('workspace-status').textContent = label + '...';
      $('workspace-status').dataset.state = 'busy';
      try {
        const result = await action();
        $('workspace-status').textContent = label + ' complete';
        $('workspace-status').dataset.state = 'ok';
        return result;
      } catch (error) {
        $('workspace-status').textContent = error.message;
        $('workspace-status').dataset.state = 'error';
        throw error;
      }
    }
    const bind = (id, label, callback) => $(id).addEventListener('click', () => task(label, callback).catch(() => {}));
    const operation = (request, project=activeRoot) => {if(!project)throw new Error('Choose a project first');return api('/api/project', {...request,project});};
    async function showOperation(id, request) {
      $(id).textContent='Running...';
      try { const result=await operation(request); $(id).textContent=JSON.stringify(result,null,2); return result; }
      catch(error) { $(id).textContent=JSON.stringify(error.result || {ok:false,error:error.message},null,2); throw error; }
    }
    async function flushDraft(snapshot=pendingDraft) {
      clearTimeout(draftTimer);
      if(!snapshot)return;
      if(!snapshot.path)throw new Error('Name the file to retain its draft');
      await operation({op:'draft_save',...snapshot},snapshot.project);
      if(pendingDraft===snapshot){pendingDraft=null;draftStored=true;$('draft-status').textContent='Draft stored. Save to apply it to the project.';}
    }
    function queueDraft() {
      dirty=true; draftStored=false;
      $('draft-status').textContent='Unsaved changes; storing draft...';
      clearTimeout(draftTimer);
      const snapshot={project:activeRoot,path:loadedFile || $('file-path').value.trim(),text:$('file-editor').value,base_sha256:fileHash || '',draft_id:draftId};
      pendingDraft=snapshot;
      draftTimer=setTimeout(()=>flushDraft(snapshot).catch(error=>{$('draft-status').textContent=error.message;}),350);
    }
    $('file-editor').addEventListener('input',queueDraft);
    window.addEventListener('beforeunload',event=>{if(dirty && !draftStored){event.preventDefault();event.returnValue='';}});
    async function projects() {
      const data = await api('/api/projects');
      const state = await api('/api/state');
      activeRoot=state.root; godotFolder=state.godot_directory || '';
      $('project').textContent = state.project;
      $('project-path').textContent = state.root;
      $('project-path').title = state.root;
      document.querySelectorAll('[data-native]').forEach(button=>{button.hidden=!state.native_package;});
      const entries = data.projects.slice();
      if (!entries.some(entry => entry.path.toLowerCase() === data.selected.toLowerCase())) entries.push({name:state.project,path:data.selected});
      $('project-select').replaceChildren();
      for (const entry of entries) {
        const option = document.createElement('option'); option.value=entry.path; option.textContent=entry.name;
        option.selected = entry.path.toLowerCase() === data.selected.toLowerCase(); $('project-select').append(option);
      }
      await api('/api/command',{command:'doctor'});
    }
    async function refreshFiles() {
      const result=await operation({op:'files'}); $('file-list').replaceChildren();
      for (const path of result.files) {
        const button=document.createElement('button'); button.textContent=path;
        button.addEventListener('click',()=>task('Read file',async()=>{$('file-path').value=path; await openFile();}).catch(()=>{}));
        $('file-list').append(button);
      }
    }
    async function openFile(savedOnly=false) {
      const path=$('file-path').value.trim(), project=activeRoot;
      $('file-editor').disabled=true;
      try {
        await flushDraft();
        const cache=await operation({op:'draft_read',path},project);
        let result;
        try {result=await operation({op:'read',path},project);} catch(error) {if(!cache.draft)throw error;result={text:'',sha256:''};}
        if(activeRoot!==project || $('file-path').value.trim()!==path)return;
        const draft=!savedOnly && cache.draft && cache.draft.text!==result.text ? cache.draft : null;
        $('file-editor').value=draft?draft.text:result.text; fileHash=draft?draft.base_sha256:result.sha256; loadedFile=path;
        dirty=Boolean(draft);draftStored=true;pendingDraft=null;
        $('draft-status').textContent=draft?'Unsaved draft restored. Save to apply it.':(cache.draft?'Saved version. A retained draft is available.':'Saved version');
      } finally {if(activeRoot===project)$('file-editor').disabled=false;}
    }
    async function agentConfig() {
      agent=await api('/api/agent');
      if ($('agent-readonly').checked) agent.mcpServers.aurum.args.push('--read-only');
      $('agent-config').textContent=JSON.stringify(agent,null,2);
    }
    function selectNode(node) {
      selected=node; $('selected-node').value=node.path;
      ['x','y','z'].forEach(axis=>{$('pos-'+axis).value=node.position?.[axis] || 0;});
      $('pos-z').disabled=node.position && !Object.prototype.hasOwnProperty.call(node.position,'z');
      $('node-script').value=node.script || '';
    }
    function renderTree(node, container, depth=0) {
      const button=document.createElement('button'); button.style.paddingLeft=(12+depth*16)+'px';
      button.textContent=node.name+'  '+node.type; button.addEventListener('click',()=>selectNode(node)); container.append(button);
      for (const child of node.children || []) renderTree(child,container,depth+1);
    }
    async function inspectScene(preserveSelection = false) {
      const previous = preserveSelection ? $('selected-node').value : '.';
      const project=activeRoot, scene=$('scene-path').value.trim();
      const result=await operation({op:'scene_inspect',scene},project);
      if(activeRoot!==project || $('scene-path').value.trim()!==scene)return;
      sceneHash=result.sha256; sceneFile=result.file_path || 'godot/'+$('scene-path').value.trim();
      $('scene-tree').replaceChildren(); renderTree(result.tree,$('scene-tree'));
      const find = node => node.path === previous ? node : (node.children || []).map(find).find(Boolean);
      selectNode(find(result.tree) || result.tree);
    }
    async function editScene(operations) {
      if (!sceneHash) throw new Error('Open the scene before editing it');
      const project=activeRoot;
      await operation({op:'scene_edit',scene:$('scene-path').value.trim(),operations,expected_sha256:sceneHash},project);
      if(activeRoot===project)await inspectScene(true);
    }
    document.querySelectorAll('[data-panel]').forEach(button=>button.addEventListener('click',()=>{
      document.querySelectorAll('[data-panel]').forEach(item=>item.setAttribute('aria-selected',String(item===button)));
      ['scene','files','agents','export'].forEach(panel=>{$('panel-'+panel).hidden=panel!==button.dataset.panel;});
      if(button.dataset.panel==='files') task('Read files',refreshFiles).catch(()=>{});
      if(button.dataset.panel==='agents') task('Agent configuration',agentConfig).catch(()=>{});
    }));
    $('project-select').addEventListener('change',()=>task('Select project',async()=>{
      $('file-editor').disabled=true;
      try {await flushDraft();}catch(error){$('file-editor').disabled=false;$('project-select').value=activeRoot;throw error;}
      await api('/api/projects',{action:'select',path:$('project-select').value}); sceneHash=null; fileHash=null; loadedFile=''; watching=false;
      $('develop').textContent='Develop'; $('scene-tree').textContent='Open a scene to inspect it.'; $('file-editor').value='';$('file-editor').disabled=false;dirty=false;draftStored=true;
      await projects(); await agentConfig();
    }).catch(()=>{}));
    function showProjectDialog(action) {
      dialogAction=action; $('project-dialog-title').textContent=action==='create'?'New project':'Import project';
      $('project-name-label').hidden=action!=='create'; $('project-template-label').hidden=action!=='create';
      $('project-dialog-error').textContent=''; $('project-dialog').showModal(); $('project-folder').focus();
    }
    $('new-project').addEventListener('click',()=>showProjectDialog('create'));
    $('import-project').addEventListener('click',()=>showProjectDialog('import'));
    $('cancel-project').addEventListener('click',()=>$('project-dialog').close());
    $('project-form').addEventListener('submit',async event=>{
      event.preventDefault();
      try {await flushDraft();}catch(error){$('project-dialog-error').textContent=error.message;return;}
      try { await task('Open project',async()=>{await api('/api/projects',{action:dialogAction,path:$('project-folder').value.trim(),name:$('project-new-name').value.trim(),template:$('project-template').value}); await projects();});
        sceneHash=null; fileHash=null; loadedFile=''; $('file-editor').value=''; $('scene-tree').textContent='Open a scene to inspect it.'; $('project-dialog').close();
      } catch(error) { $('project-dialog-error').textContent=error.message; }
    });
    bind('develop','Development',async()=>{await api('/api/command',{command:watching?'end-develop':'develop'}); watching=!watching; $('develop').textContent=watching?'Stop watching':'Develop';});
    bind('validate-project','Validation',()=>operation({op:'validate'}));
    bind('package-project','Package Windows app',async()=>{const result=await operation({op:'package',output:$('package-path').value.trim()});$('export-result').textContent=JSON.stringify(result,null,2);});
    bind('refresh-presets','Read export presets',async()=>{$('export-result').textContent=JSON.stringify(await operation({op:'presets'}),null,2);});
    bind('configure-export','Configure export',async()=>{const result=await operation({op:'configure_export',platform:$('export-platform').value});$('export-preset').value=result.preset;$('export-result').textContent=JSON.stringify(result,null,2);});
    bind('export-project','Platform export',()=>showOperation('export-result',{op:'export',preset:$('export-preset').value.trim(),output:$('platform-output').value.trim(),debug:$('export-debug').checked}));
    bind('headless-play','Headless playtest',async()=>{const args=JSON.parse($('test-args').value);if(!Array.isArray(args))throw new Error('Game arguments must be a JSON array of strings');const report=$('test-report').checked;const request={op:'play',frames:report||args.length?36000:120,fixed_fps:60,user_args:args,report};const scene=$('test-scene').value.trim();if(scene)request.scene=scene;await showOperation('test-result',request);});
    bind('refresh-files','Read files',refreshFiles); bind('open-file','Read file',openFile);
    bind('open-saved-file','Read saved version',()=>openFile(true));
    bind('restore-draft','Restore draft',()=>openFile(false));
    bind('new-file','New file',async()=>{await flushDraft();loadedFile='';fileHash=null;dirty=false;draftStored=true;$('file-path').value=(godotFolder?godotFolder+'/':'')+'new_script.gd';$('file-editor').value='';$('file-editor').disabled=false;$('draft-status').textContent='New file';$('file-path').focus();});
    bind('save-file','Save file',async()=>{
      const path=$('file-path').value.trim(),project=activeRoot;
      if (path!==loadedFile && fileHash) throw new Error('Open the selected path before overwriting it');
      clearTimeout(draftTimer);
      $('file-editor').disabled=true;
      try {
        await operation({op:'write',path,text:$('file-editor').value,expected_sha256:path===loadedFile?fileHash:''},project);
        pendingDraft=null;
        await operation({op:'draft_clear',path,draft_id:draftId},project);
        if(activeRoot===project){dirty=false;draftStored=true;await openFile(true);await refreshFiles();}
      } finally {if(activeRoot===project)$('file-editor').disabled=false;}
    });
    bind('undo-file','Undo file',async()=>{await operation({op:'undo',path:$('file-path').value.trim(),expected_sha256:fileHash});await openFile(true);});
    bind('inspect-scene','Open scene',inspectScene);
    bind('set-main-scene','Set start scene',()=>operation({op:'set_main_scene',scene:$('scene-path').value.trim()}));
    bind('create-scene','Create scene',async()=>{await operation({op:'scene_create',scene:$('scene-path').value.trim(),name:'Main',root_type:'Node3D'});await inspectScene();});
    bind('undo-scene','Undo scene',async()=>{if(!sceneFile)throw new Error('Open a scene first');await operation({op:'undo',path:sceneFile,expected_sha256:sceneHash});await inspectScene();});
    bind('add-node','Add node',()=>{
      const type=$('node-type').value; const properties=type==='MeshInstance3D'?{mesh:{resource:'BoxMesh'}}:{};
      return editScene([{op:'create',parent:$('selected-node').value,name:$('node-name').value,type,properties}]);
    });
    bind('apply-position','Move node',()=>{const position={x:Number($('pos-x').value),y:Number($('pos-y').value)};if(!$('pos-z').disabled)position.z=Number($('pos-z').value);return editScene([{op:'set',node:$('selected-node').value,properties:{position}}]);});
    bind('apply-properties','Set properties',()=>editScene([{op:'set',node:$('selected-node').value,properties:JSON.parse($('node-properties').value)}]));
    bind('attach-script','Attach script',()=>editScene([{op:'attach_script',node:$('selected-node').value,script:$('node-script').value}]));
    bind('remove-node','Remove node',()=>{if(!selected||selected.path==='.')throw new Error('Select a child node');return editScene([{op:'remove',node:selected.path}]);});
    bind('copy-agent','Copy configuration',async()=>{await agentConfig();await navigator.clipboard.writeText(JSON.stringify(agent,null,2));});
    $('agent-readonly').addEventListener('change',()=>agentConfig().catch(()=>{}));
    $('shutdown').addEventListener('click',event=>{
      event.preventDefault();event.stopImmediatePropagation();
      task('Close Studio',async()=>{await flushDraft();await api('/api/stop',{});}).catch(()=>{});
    },true);
    task('Load workspace',async()=>{await projects();await agentConfig();if(!$('panel-files').hidden)await refreshFiles();}).catch(()=>{});
  };
  if(document.readyState==='loading') document.addEventListener('DOMContentLoaded',ready); else ready();
})();
