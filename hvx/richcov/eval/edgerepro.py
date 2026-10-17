import json,os,sys
A,B=sys.argv[1],sys.argv[2]
a=[json.loads(l) for l in open(A+"/runs.jsonl")]
b={json.loads(l)["tag"]:json.loads(l) for l in open(B+"/runs.jsonl")}
n=same=0; tot={}
for x in a:
  y=b.get(x["tag"])
  if not y or x["cov"].get("truncated") or y["cov"].get("truncated"): continue
  d=[]
  for p in ("hydostack","redis","mysqld"):
    e1="%s/cov/%s/%s.edges"%(A,x["tag"],p); e2="%s/cov/%s/%s.edges"%(B,y["tag"],p)
    if not (os.path.exists(e1) and os.path.exists(e2)): d=None; break
    s1=set(open(e1).read().split("\n")); s2=set(open(e2).read().split("\n"))
    if s1!=s2: d.append("%s:%d" % (p, len(s1^s2)))
  if d is None: continue
  n+=1; same+= not d
  if d: print(x["tag"], d)
print("compared",n,"identical",same)
