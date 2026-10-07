import test from 'node:test';
import assert from 'node:assert/strict';
import {PassThrough} from 'node:stream';
import {buildRequest,run,errorStatus} from './claude-agent.mjs';

const credential={type:'oauth',token:'fixture-token'};
const basic={model:'claude-sonnet-4-6',max_tokens:256,system:'system instruction',messages:[{role:'user',content:'hello'}]};
const start=(usage={input_tokens:7,output_tokens:1})=>({type:'message_start',message:{id:'msg_x',type:'message',role:'assistant',model:basic.model,content:[],usage}});
const finish=(reason='end_turn',usage={input_tokens:7,output_tokens:3})=>[{type:'message_delta',delta:{stop_reason:reason},usage},{type:'message_stop'}];
const text=[{type:'content_block_start',index:0,content_block:{type:'text',text:''}},{type:'content_block_delta',index:0,delta:{type:'text_delta',text:'hello'}},{type:'content_block_stop',index:0}];
async function fakeRun(body,frames) {
 let options,closed=false,content='';
 const stream=new PassThrough();stream.on('data',x=>content+=x);
 const result=(async function*(){for(const event of frames) yield event.sdk ?? {type:'stream_event',event};})();result.close=()=>{closed=true;};
 await run({body,credential,upstream:'https://api.anthropic.com'}, {McpServer:class {constructor(){this.server={setRequestHandler(){}};}}, query(input){options=input.options;return result;}},stream);
 return {options,closed,content,header:JSON.parse(content.split('\n')[0]),events:content.split('\n').filter(s=>s.startsWith('data: ')).map(s=>JSON.parse(s.slice(6)))};
}
test('settings, credentials, process env and disabled builtins are isolated',()=>{
 const {options}=buildRequest(basic,credential,'https://api.anthropic.com','/private/tmp/isolated',{PATH:'/bin',HOME:'/secret',ANTHROPIC_AUTH_TOKEN:'proxy-key',ANTHROPIC_API_KEY:'wrong-key',CLAUDECODE:'nested',NODE_OPTIONS:'--import=malicious',HTTP_PROXY:'http://recursive',OPENAI_API_KEY:'other-key'});
 assert.deepEqual(options.tools,[]);assert.deepEqual(options.settingSources,[]);assert.deepEqual(options.plugins,[]);assert.deepEqual(options.skills,[]);
 assert.equal(options.env.CLAUDE_CODE_OAUTH_TOKEN,'fixture-token');assert.equal(options.env.HOME,'/private/tmp/isolated');
 for(const key of ['ANTHROPIC_AUTH_TOKEN','ANTHROPIC_API_KEY','CLAUDECODE','NODE_OPTIONS','HTTP_PROXY','OPENAI_API_KEY'])assert.equal(options.env[key],undefined);
 assert.equal(options.env.CLAUDE_CODE_MAX_OUTPUT_TOKENS,'256');assert.deepEqual(options.thinking,{type:'disabled'});
});
test('full structured transcript retains roles, images, tool IDs and JSON schema',async()=>{
 const image={type:'image',source:{type:'base64',media_type:'image/png',data:'fixture'}};
 const tool={name:'exec_command',description:'Execute remotely',input_schema:{type:'object',properties:{cmd:{type:'string'}},required:['cmd'],additionalProperties:false}};
 const messages=[{role:'user',content:[{type:'text',text:'inspect'},image]},{role:'assistant',content:[{type:'tool_use',id:'call_42',name:tool.name,input:{cmd:'pwd'}}]},{role:'user',content:[{type:'tool_result',tool_use_id:'call_42',content:[{type:'text',text:'/project'},image]}]}];
 const {options,prompt,tools}=buildRequest({...basic,messages,tools:[tool]},credential,'https://api.anthropic.com','/tmp/isolated');
 const entries=await options.sessionStore.load();
 assert.deepEqual(entries.map(e=>e.message.role),['user','assistant','user']);assert.deepEqual(entries[0].message.content,messages[0].content);
 assert.equal(entries[1].message.content[0].name,'mcp__llmux__exec_command');assert.equal(entries[1].message.content[0].id,'call_42');
 assert.equal(entries[1].parentUuid,entries[0].uuid);assert.deepEqual(entries[2].message,messages[2]);assert.equal(options.env.CLAUDE_CODE_RESUME_INTERRUPTED_TURN,'1');assert.deepEqual(tools,[tool]);
});
test('single model turn usage is merged, not doubled; SDK closes',async()=>{
 const result=await fakeRun(basic,[start(),...text,...finish()]);
 assert.equal(result.header.status,200);assert.equal(result.closed,true);
 assert.deepEqual(result.events.find(e=>e.type==='message_delta').usage,{input_tokens:7,output_tokens:3});
});
test('incomplete max_tokens stop reason survives',async()=>{
 const result=await fakeRun(basic,[start(),...text,...finish('max_tokens')]);
 assert.equal(result.events.find(e=>e.type==='message_delta').delta.stop_reason,'max_tokens');
});
test('registered client tool names are restored and executable inputs unchanged',async()=>{
 const result=await fakeRun({...basic,tools:[{name:'exec_command',input_schema:{type:'object'}}]},[start(),{type:'content_block_start',index:0,content_block:{type:'tool_use',id:'call_z',name:'mcp__llmux__exec_command',input:{}}},{type:'content_block_delta',index:0,delta:{type:'input_json_delta',partial_json:'{"cmd":"pwd"}'}},{type:'content_block_stop',index:0},...finish('tool_use')]);
 assert.deepEqual(result.events[1].content_block,{type:'tool_use',id:'call_z',name:'exec_command',input:{}});
 assert.equal(result.events[2].delta.partial_json,'{"cmd":"pwd"}');
});
test('WebSearch executes internally, indexes continue, usage sums distinct rounds',async()=>{
 const result=await fakeRun({...basic,_llmux_web_search:true},[start(),{type:'content_block_start',index:0,content_block:{type:'tool_use',id:'internal',name:'WebSearch',input:{}}},{type:'content_block_stop',index:0},...finish('tool_use'),start({input_tokens:9,output_tokens:1}),...text,...finish('end_turn',{input_tokens:9,output_tokens:5})]);
 assert.equal(result.events.filter(e=>e.type==='message_start').length,1);
 assert.equal(result.events.filter(e=>e.type==='message_stop').length,1);
 assert.ok(result.events.every(e=>e.content_block?.name!=='WebSearch'));
 assert.deepEqual(result.events.find(e=>e.type==='message_delta').usage,{input_tokens:16,output_tokens:8});
 assert.deepEqual(await result.options.canUseTool('WebSearch',{query:'evidence'}),{behavior:'allow',updatedInput:{query:'evidence'}});
});
test('unsupported tool choices fail instead of weakening client requirement',()=>{
 assert.throws(()=>buildRequest({...basic,tool_choice:{type:'any'}},credential,'url','/tmp'),/tool choice/);
});

test('unsupported controls fail explicitly without creating an SDK query',async()=>{
 const result=await fakeRun({...basic,temperature:0.2},[]);
 assert.equal(result.header.status,400);assert.equal(result.options,undefined);
});
test('truncated SDK iterable fails rather than claiming completion',async()=>{
 const result=await fakeRun(basic,[start(),...text]);
 assert.equal(result.events.at(-1).type,'error');
 assert.ok(!result.events.some(e=>e.type==='message_stop'));
});

test('tool choice none disables external and internal WebSearch tools',async()=>{
 const body={...basic,_llmux_web_search:true,tools:[{name:'exec_command'}],tool_choice:{type:'none'}};
 const built=buildRequest(body,credential,'https://api.anthropic.com','/tmp/isolated');
 assert.deepEqual(built.tools,[]);assert.deepEqual(built.options.tools,[]);assert.equal(built.webSearch,false);
 const result=await fakeRun(body,[start(),...text,...finish()]);
 assert.equal((await result.options.canUseTool('WebSearch',{query:'ignored'})).behavior,'deny');
});

const sdkErrorCases = [
 ['authentication_failed',401],['oauth_org_not_allowed',403],['account_on_hold',403],
 ['verification_required',403],['billing_error',402],['rate_limit',429],
 ['overloaded',503],['invalid_request',400],['model_not_found',404],
 ['server_error',500],['unknown',502],['max_output_tokens',502],['cloud_credential_error',503],
];
for (const [code,status] of sdkErrorCases) {
 test(`SDK error ${code} maps to ${status} before streaming`,async()=>{
  assert.equal(errorStatus(code),status);assert.equal(errorStatus({error:code}),status);
  const result=await fakeRun(basic,[{sdk:{type:'assistant',error:code}}]);
  assert.deepEqual(result.header,{status,error_code:code});
  assert.equal(result.events.length,0);assert.equal(result.closed,true);
 });
}
test('unknown SDK strings never become trusted error codes or leak details',async()=>{
 const secret='fixture-credential-should-not-appear';
 const result=await fakeRun(basic,[{sdk:{type:'assistant',error:secret}}]);
 assert.deepEqual(result.header,{status:502,error_code:'unknown'});
 assert.ok(!result.content.includes(secret));assert.equal(errorStatus('__proto__'),502);
});
test('raw API error before message_start has an HTTP failure status',async()=>{
 const result=await fakeRun(basic,[{type:'error',error:{type:'authentication_error',message:'secret-body'}}]);
 assert.deepEqual(result.header,{status:401});assert.equal(result.events.length,0);
 assert.ok(!result.content.includes('secret-body'));
});
test('post-start failure remains SSE and never advertises account failover',async()=>{
 const result=await fakeRun(basic,[start(),{sdk:{type:'assistant',error:'oauth_org_not_allowed'}}]);
 assert.deepEqual(result.header,{status:200});assert.equal(result.events.at(-1).type,'error');
 assert.ok(!result.events.some(e=>e.type==='message_stop'));
});
test('synthetic max_output_tokens diagnostic preserves real partial stream',async()=>{
 const result=await fakeRun(basic,[start(),...text,{sdk:{type:'assistant',error:'max_output_tokens'}},...finish('max_tokens')]);
 assert.deepEqual(result.header,{status:200});assert.equal(result.events.at(-1).type,'message_stop');
 assert.equal(result.events.find(e=>e.type==='message_delta').delta.stop_reason,'max_tokens');
 assert.ok(!result.events.some(e=>e.type==='error'));
});
