// Network-free inference: the actual pinned SDK and native Claude executable
// talk only to this test's mock Anthropic endpoint. Run with an installed bridge.
import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import {spawn} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {dirname} from 'node:path';
const runtime=process.env.LLMUX_CLAUDE_SDK_DIR ?? dirname(fileURLToPath(import.meta.url));
const bridge=fileURLToPath(new URL('./claude-agent.mjs',import.meta.url));
const tool={name:'exec_command',description:'Execute a command in the client sandbox',input_schema:{type:'object',properties:{cmd:{type:'string'},timeout:{type:'number'}},required:['cmd'],additionalProperties:false}};
function frames(kind='text', structuredValue){
 const block=kind==='structured'?{type:'tool_use',id:'structured_sdk',name:'StructuredOutput',input:{}}:kind==='tool'?{type:'tool_use',id:'call_native_sdk',name:'mcp__llmux__exec_command',input:{}}:{type:'text',text:''};
 return [{type:'message_start',message:{id:'msg_mock',type:'message',role:'assistant',model:'claude-sonnet-4-6',content:[],stop_reason:null,stop_sequence:null,usage:{input_tokens:17,output_tokens:1}}},{type:'content_block_start',index:0,content_block:block},{type:'content_block_delta',index:0,delta:kind==='structured'?{type:'input_json_delta',partial_json:JSON.stringify({output:structuredValue})}:kind==='tool'?{type:'input_json_delta',partial_json:'{"cmd":"printf sdk_fixture"}'}:{type:'text_delta',text:'SDK fixture complete'}},{type:'content_block_stop',index:0},{type:'message_delta',delta:{stop_reason:(kind==='tool'||kind==='structured')?'tool_use':kind==='max_tokens'?'max_tokens':'end_turn',stop_sequence:null},usage:{input_tokens:17,output_tokens:5}},{type:'message_stop'}];
}
async function execute(body,kind='text',structuredValue) {
 const requests=[];
 const server=http.createServer(async(req,res)=>{
  let raw='';for await(const c of req)raw+=c;
  requests.push({url:req.url,body:JSON.parse(raw),headers:req.headers});
  if(typeof kind==='object'){res.writeHead(kind.status,{'content-type':'application/json'});res.end(JSON.stringify({type:'error',error:kind.error}));return;}
  if(kind==='401'||kind==='429'){res.writeHead(Number(kind),{'content-type':'application/json'});res.end(JSON.stringify({type:'error',error:{type:kind==='401'?'authentication_error':'rate_limit_error',message:'fixture failure'}}));return;}
  res.writeHead(200,{'content-type':'text/event-stream'});res.end(frames(kind,structuredValue).map(f=>`event: ${f.type}\ndata: ${JSON.stringify(f)}\n\n`).join(''));
 });
 await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
 const child=spawn(process.execPath,[bridge,runtime],{stdio:['pipe','pipe','pipe']});
 let output='',stderr='';child.stdout.on('data',x=>output+=x);child.stderr.on('data',x=>stderr+=x);
 child.stdin.end(JSON.stringify({body,credential:{type:'apikey',token:'fixture-not-a-real-secret'},upstream:`http://127.0.0.1:${server.address().port}`}));
 const timeout=setTimeout(()=>child.kill('SIGKILL'),30000);
 try {await new Promise((resolve,reject)=>{child.on('error',reject);child.on('exit',resolve);});}
 finally {clearTimeout(timeout);server.close();}
 assert.equal(stderr,'');
 return {requests,output,header:JSON.parse(output.split('\n')[0]),status:JSON.parse(output.split('\n')[0]).status,events:output.split('\n').filter(l=>l.startsWith('data: ')).map(l=>JSON.parse(l.slice(6)))};
}
const body={model:'claude-sonnet-4-6',max_tokens:512,system:'Use only client tools',tools:[tool],messages:[{role:'user',content:'Run printf using the client'}]};
test('actual SDK retains tool JSON schema and yields external call without execution',{timeout:40000},async()=>{
 const result=await execute(body,'tool');assert.equal(result.status,200);assert.equal(result.requests.length,1);
 const wire=result.requests[0].body;const schema=wire.tools.find(t=>t.name==='mcp__llmux__exec_command');
 assert.ok(schema,'registered exact client tool');assert.deepEqual(schema.input_schema,tool.input_schema);assert.equal(wire.max_tokens,512);assert.deepEqual(wire.thinking,{type:'disabled'});
 assert.equal(wire.tools.length,1,'no built-in tools');assert.equal(result.requests[0].headers['x-api-key'],'fixture-not-a-real-secret');
 assert.equal(result.events.find(e=>e.type==='content_block_start').content_block.name,'exec_command');
 assert.equal(result.events.find(e=>e.type==='content_block_start').content_block.id,'call_native_sdk');
 assert.deepEqual(result.events.find(e=>e.type==='message_delta').usage,{input_tokens:17,output_tokens:5});
});
test('actual SDK replays assistant/client result with exact IDs and ordered text/image',{timeout:40000},async()=>{
 const image={type:'image',source:{type:'base64',media_type:'image/png',data:'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+XvN8AAAAASUVORK5CYII='}};
 const result=await execute({...body,messages:[{role:'user',content:[{type:'text',text:'prior question'},image]},{role:'assistant',content:[{type:'text',text:'prior note'},{type:'tool_use',id:'call_native_sdk',name:'exec_command',input:{cmd:'printf sdk_fixture'}}]},{role:'user',content:[{type:'tool_result',tool_use_id:'call_native_sdk',content:[{type:'text',text:'sdk_fixture'},image]}]}]});
 assert.equal(result.status,200);assert.equal(result.requests.length,1);
 const messages=result.requests[0].body.messages;
 assert.deepEqual(messages.map(m=>m.role),['user','assistant','user']);
 assert.equal(messages[0].content.find(b=>b.type==='text').text,'prior question');assert.deepEqual(messages[0].content.find(b=>b.type==='image').source,image.source);
 const call=messages[1].content.find(b=>b.type==='tool_use');assert.equal(call.id,'call_native_sdk');assert.equal(call.name,'mcp__llmux__exec_command');assert.deepEqual(call.input,{cmd:'printf sdk_fixture'});
 const output=messages[2].content.find(b=>b.type==='tool_result');assert.equal(output.tool_use_id,call.id);assert.equal(output.content[0].text,'sdk_fixture');assert.deepEqual(output.content[1].source,image.source);
});
test('actual SDK 401 remains an authentication status before stream',{timeout:40000},async()=>{
 const result=await execute({...body,tools:[]},'401');assert.equal(result.status,401);assert.equal(result.requests.length,1);assert.ok(!result.output.includes('fixture-not-a-real-secret'));
});

test('actual SDK rate limit reaches shared scheduler as HTTP 429',{timeout:40000},async()=>{
 const result=await execute({...body,tools:[]},'429');assert.equal(result.status,429);assert.equal(result.requests.length,1);
});

test('actual SDK returns schema-validated JSON instead of internal StructuredOutput calls',{timeout:40000},async()=>{
 const schema={type:'object',properties:{value:{type:'string'}},required:['value'],additionalProperties:false};
 const value={value:'schema_fixture'};
 const result=await execute({...body,tools:[],_llmux_output_format:{type:'json_schema',schema}},'structured',value);
 assert.equal(result.status,200);assert.equal(result.requests.length,1);
 assert.ok(result.requests[0].body.tools.some(t=>t.name==='StructuredOutput'));
 assert.ok(!result.events.some(e=>e.content_block?.type==='tool_use'));
 const text=result.events.filter(e=>e.delta?.type==='text_delta').map(e=>e.delta.text).join('');
 assert.deepEqual(JSON.parse(text),value);assert.equal(result.events.at(-1).type,'message_stop');
});
for (const [type,value] of [['array',['one',2]],['string','scalar'],['number',7],['boolean',false],['null',null]]) {
 test(`actual SDK structured result preserves ${type}`,{timeout:40000},async()=>{
  const schema={type};
  const result=await execute({...body,tools:[],_llmux_output_format:{type:'json_schema',schema}},'structured',value);
  assert.equal(result.status,200);assert.equal(result.requests.length,1);
  const text=result.events.filter(e=>e.delta?.type==='text_delta').map(e=>e.delta.text).join('');
  assert.deepEqual(JSON.parse(text),value);assert.equal(result.events.at(-1).type,'message_stop');
 });
}
test('actual SDK honors the requested 1m context suffix',{timeout:40000},async()=>{
 const result=await execute({...body,model:'claude-sonnet-4-6[1m]',tools:[]});
 assert.equal(result.status,200);assert.equal(result.requests.length,1);
 assert.equal(result.requests[0].body.model,'claude-sonnet-4-6');
 assert.match(result.requests[0].headers['anthropic-beta'] ?? '',/context-1m/);
});
test('structured schema references retain their original resource root',{timeout:40000},async()=>{
 const schema={type:'array',items:{$ref:'#/$defs/item'},$defs:{item:{type:'string'}}};
 const result=await execute({...body,tools:[],_llmux_output_format:{type:'json_schema',schema}},'structured',['referenced']);
 assert.equal(result.requests.length,1);
 const text=result.events.filter(e=>e.delta?.type==='text_delta').map(e=>e.delta.text).join('');
 assert.deepEqual(JSON.parse(text),['referenced']);
});
test('structured output still hands client tool calls back to Codex',{timeout:40000},async()=>{
 const result=await execute({...body,_llmux_output_format:{type:'json_schema',schema:{type:'object'}}},'tool');
 assert.equal(result.requests.length,1);
 assert.equal(result.events.find(e=>e.type==='content_block_start').content_block.name,'exec_command');
 assert.equal(result.events.find(e=>e.type==='message_delta').delta.stop_reason,'tool_use');
});

test('actual SDK organization OAuth restriction preserves permanent account code',{timeout:40000},async()=>{
 const result=await execute({...body,tools:[]},{status:403,error:{type:'permission_error',message:'OAuth authentication is currently not allowed for this organization'}});
 assert.deepEqual(result.header,{status:403,error_code:'oauth_org_not_allowed'});
 assert.equal(result.requests.length,1);assert.equal(result.events.length,0);
 assert.ok(!result.output.includes('fixture-not-a-real-secret'));
});
test('actual SDK model not found remains a request error without account rejection',{timeout:40000},async()=>{
 const result=await execute({...body,tools:[]},{status:404,error:{type:'not_found_error',message:'model does not exist'}});
 assert.deepEqual(result.header,{status:404,error_code:'model_not_found'});
 assert.equal(result.events.length,0);
});
test('actual SDK output limit preserves partial text and max_tokens termination',{timeout:40000},async()=>{
 const result=await execute({...body,tools:[]},'max_tokens');
 assert.deepEqual(result.header,{status:200});assert.equal(result.requests.length,1);
 assert.equal(result.events.find(e=>e.type==='message_delta').delta.stop_reason,'max_tokens');
 assert.equal(result.events.find(e=>e.delta?.type==='text_delta').delta.text,'SDK fixture complete');
 assert.ok(!result.events.some(e=>e.type==='error'));
});
test('actual SDK verification restriction preserves permanent account code',{timeout:40000},async()=>{
 const result=await execute({...body,tools:[]},{status:403,error:{type:'permission_error',message:'Organization verification required',details:{error_code:'verification_required'}}});
 assert.deepEqual(result.header,{status:403,error_code:'verification_required'});
 assert.equal(result.requests.length,1);assert.equal(result.events.length,0);
});
test('actual SDK exhausted credit balance retains billing status',{timeout:40000},async()=>{
 const result=await execute({...body,tools:[]},{status:400,error:{type:'invalid_request_error',message:'Credit balance is too low'}});
 assert.deepEqual(result.header,{status:402,error_code:'billing_error'});
 assert.equal(result.requests.length,1);assert.equal(result.events.length,0);
});
