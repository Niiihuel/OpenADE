import re,glob,sys,json,os
def literals(src):
    out=[];i=0;n=len(src)
    while i<n:
        c=src[i]
        if src.startswith('//',i):
            j=src.find('\n',i); i=n if j<0 else j; continue
        if src.startswith('/*',i):
            j=src.find('*/',i); i=n if j<0 else j+2; continue
        if c=="'":
            m=re.match(r"'(\\.[^']*|[^'\\])'",src[i:])
            if m: i+=m.end(); continue
            i+=1; continue
        m=re.match(r'b?r(#*)"',src[i:])
        if m and (i==0 or not (src[i-1].isalnum() or src[i-1]=='_')):
            h=m.group(1); end=src.find('"'+h,i+m.end())
            if end<0: break
            out.append((i,src[i+m.end():end])); i=end+1+len(h); continue
        if c=='"':
            j=i+1;buf=[]
            while j<n and src[j]!='"':
                if src[j]=='\\':
                    e=src[j+1]
                    if e=='n': buf.append('\n'); j+=2
                    elif e=='t': buf.append('\t'); j+=2
                    elif e in '"\\\'': buf.append(e); j+=2
                    elif e=='\n':
                        j+=2
                        while j<n and src[j] in ' \t\n': j+=1
                    elif e=='u':
                        k=src.find('}',j); buf.append(chr(int(src[j+3:k],16))); j=k+1
                    elif e=='x': buf.append(chr(int(src[j+2:j+4],16))); j+=4
                    elif e=='0': buf.append('\0'); j+=2
                    elif e=='r': buf.append('\r'); j+=2
                    else: buf.append(e); j+=2
                else: buf.append(src[j]); j+=1
            out.append((i,''.join(buf))); i=j+1; continue
        i+=1
    return out
SP=re.compile(r'[áéíóúñ¿¡ÁÉÍÓÚÑü]')
WORDS=set('el la los las de del para con sin que no un una este esta esto al por se es en y o su sus más ya hay todo nuevo nueva abrir cerrar guardar archivo archivos cambios tarea tareas proyecto modelo agente error buscar cancelar eliminar copiar ver usar activar desactivar actualizar instalar añadir agregar quitar configurar editar ajustes cargando listo sesión conversación'.split())
def spanish(t):
    if not re.search(r'[A-Za-zÁ-ú]{2}',t): return False
    if re.fullmatch(r'[a-z0-9_:./\-{}#*@=,\[\]]+',t): return False
    if SP.search(t): return True
    if re.fullmatch(r"[A-ZÁÉÍÓÚÑ][a-záéíóúñü]+( [A-Za-záéíóúñü]+){0,3}[.…:]?", t): return True
    w=[x.lower() for x in re.findall(r"[A-Za-zÁ-úñ]+",t)]
    if not w: return False
    hits=sum(1 for x in w if x in WORDS)
    return hits>=1 and (len(w)<=3 or hits/len(w)>=0.2)
files=[p for p in glob.glob('apps/agents-window/src/**/*.rs',recursive=True)+glob.glob('crates/*/src/**/*.rs',recursive=True) if not re.search(r'tests?\.rs$|/tests/|_tests\.rs$|visual_qa|e2e',p)]
found={}
for p in files:
    src=open(p,errors='ignore').read()
    cut=len(src)
    m=re.search(r'\n#\[cfg\(test\)\]\s*\n(pub\(crate\) |pub )?mod \w+ \{',src)
    if m: cut=m.start()
    for pos,t in literals(src):
        if pos>=cut: continue
        t=t.strip('\n')
        if 2<=len(t)<=600 and spanish(t):
            found.setdefault(t,p)
json.dump(found,open(sys.argv[1],'w'),ensure_ascii=False,indent=0)
print(len(found), len(files))
tm=[t for t in found if '{' in t]
print('templates',len(tm),'chars',sum(len(t) for t in found))
