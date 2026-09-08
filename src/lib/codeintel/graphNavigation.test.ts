import { describe, expect, it } from "vitest";
import { buildCodeGraphModel } from "./graphPayload";
import { buildGraphIndex, filterGraphNodes, fitGraphView, graphNodeOpenPath, traceGraph } from "./graphNavigation";

const model = buildCodeGraphModel({nodes:[
  {id:"a",name:"A",path:"src/a.swift",community:"core"},
  {id:"b",name:"B",path:"src/b.rs",community:"core"},
  {id:"c",name:"C",path:"tests/c.rs",community:"tests"},
  {id:"d",name:"D",path:"generated/d.ts",community:"generated"},
  {id:"e",name:"E",path:"src/e.swift",community:"core"},
],links:[{source:"a",target:"b"},{source:"b",target:"c"},{source:"c",target:"a"},{source:"d",target:"a"}]});

describe("graph exploration", () => {
  it("fits translated and extreme supplied coordinates into the actual viewport", () => {
    const model = buildCodeGraphModel({nodes:[{id:"a",name:"A",x:-1e7,y:1e7},{id:"b",name:"B",x:1e7,y:-1e7}],links:[]});
    for (const [width,height] of [[320,240],[1440,900]]) {
      const camera=fitGraphView(model.nodes,width,height);
      for (const n of model.nodes) {
        expect((n.x-n.radius)*camera.scale+camera.panX).toBeGreaterThanOrEqual(24);
        expect((n.x+n.radius)*camera.scale+camera.panX).toBeLessThanOrEqual(width-24);
        expect((n.y-n.radius)*camera.scale+camera.panY).toBeGreaterThanOrEqual(24);
        expect((n.y+n.radius)*camera.scale+camera.panY).toBeLessThanOrEqual(height-24);
      }
    }
    expect(fitGraphView([],0,0)).toEqual({scale:1,panX:0,panY:0});
  });
  it("traverses undirected subsystem neighbors from either end without inventing dependency direction", () => {
    const neighbors = buildCodeGraphModel({nodes:[{id:"core",name:"Core"},{id:"ui",name:"UI"}],links:[{source:"core",target:"ui",kind:"neighbor"}]});
    const index = buildGraphIndex(neighbors);
    for (const root of ["core","ui"]) for (const direction of ["incoming","outgoing","both"] as const) {
      expect([...traceGraph(index,root,direction).ids].sort()).toEqual(["core","ui"]);
    }
    expect(filterGraphNodes(neighbors,index,{connectedOnly:true})).toHaveLength(2);
  });
  it("follows explicit direction and bounded hop depth through cycles", () => {
    const index = buildGraphIndex(model);
    expect([...traceGraph(index,"a","outgoing",1).ids].sort()).toEqual(["a","b"]);
    expect([...traceGraph(index,"a","incoming",1).ids].sort()).toEqual(["a","c","d"]);
    expect([...traceGraph(index,"a","outgoing",2).ids].sort()).toEqual(["a","b","c"]);
    expect([...traceGraph(index,"a","both",3).ids].sort()).toEqual(["a","b","c","d"]);
    expect(traceGraph(index,"missing","both",3).ids.size).toBe(0);
    expect([...traceGraph(index,"a","outgoing",3,new Set(["a","c"])).ids]).toEqual(["a"]);
    expect([...traceGraph(index,"a","incoming",1).edges].sort()).toEqual([2,3]);
  });

  it("keeps capped results stable when producer order changes", () => {
    const nodes=Array.from({length:1200},(_,i)=>({id:`n${i}`,name:`N${i}`}));
    const links=nodes.slice(1).map(n=>({source:"n0",target:n.id}));
    const first=traceGraph(buildGraphIndex(buildCodeGraphModel({nodes,links})),"n0","outgoing",1);
    const reversed=traceGraph(buildGraphIndex(buildCodeGraphModel({nodes:[...nodes].reverse(),links:[...links].reverse()})),"n0","outgoing",1);
    expect([...reversed.ids]).toEqual([...first.ids]);
  });

  it("limits dense traces explicitly, including invalid numeric limits", () => {
    const nodes=Array.from({length:3000},(_,i)=>({id:`n${i}`,name:`N${i}`}));
    const dense=buildGraphIndex(buildCodeGraphModel({nodes,links:nodes.slice(1).map(n=>({source:"n0",target:n.id}))}));
    const trace=traceGraph(dense,"n0","both",Infinity);
    expect(trace.ids.size).toBe(1000);
    expect(trace.truncated).toBe(true);
    expect(trace.visitedEdges).toBeLessThanOrEqual(50_000);
    expect(traceGraph(dense,"n0","both",NaN).ids.size).toBe(1000);
  });

  it("combines language, role, community, connectivity, and text filters", () => {
    const index=buildGraphIndex(model);
    expect(filterGraphNodes(model,index,{language:"swift",connectedOnly:true}).map(n=>n.id)).toEqual(["a"]);
    expect(filterGraphNodes(model,index,{hideTests:true,hideGenerated:true}).map(n=>n.id)).toEqual(["a","b","e"]);
    expect(filterGraphNodes(model,index,{community:"core",query:"B.RS"}).map(n=>n.id)).toEqual(["b"]);
    expect(filterGraphNodes(model,index,{query:"not present"})).toEqual([]);
  });
});

describe("graph file activation", () => {
  it.each(["main.swift","Main.java","source.cpp","module.kt","Makefile",".gitignore","LICENSE","nested/with spaces/file.c","src/文档.rs"])("opens indexed file %s without a language whitelist", path => {
    expect(graphNodeOpenPath({id:path,name:path,kind:"file"})).toBe(path);
  });
  it("uses explicit paths and rejects nodes without a file identity", () => {
    expect(graphNodeOpenPath({id:"opaque",name:"Method",kind:"function",path:"main.swift"})).toBe("main.swift");
    expect(graphNodeOpenPath({id:"area/core",name:"Core",kind:"subsystem"})).toBeNull();
    expect(graphNodeOpenPath({id:"opaque",name:"Method",kind:"function"})).toBeNull();
    expect(graphNodeOpenPath({id:"src/a.rs::run",name:"run",kind:"function"})).toBe("src/a.rs");
  });
});
