"use client";

import { useEffect } from "react";

const TIMING_CODE = `(function(){
  function fmt(ms){return ms>=1000?(ms/1000).toFixed(1)+'s':Math.round(ms)+'ms'}
  function emit(data){
    window.__ssrTiming=data;
    window.dispatchEvent(new Event('ssr-timing'));
  }

  var ssrReady = false;

  // Reset timing on back/forward navigation (bfcache or SPA)
  window.addEventListener('popstate',function(){
    ssrReady = true;
    emit({server:'--ms'});
  });

  // --- Full page load timing (SSR) ---
  function updateSSR(){
    var nav=performance.getEntriesByType('navigation')[0];
    if(!nav||nav.responseStart===0){setTimeout(updateSSR,10);return}
    emit({server:fmt(nav.responseStart-nav.requestStart)});
    var c=setInterval(function(){
      if(nav.responseEnd>0){clearInterval(c);
        emit({server:fmt(nav.responseEnd-nav.requestStart)});
      }
    },50);
    function onLoaded(){
      if(nav.responseEnd===0){setTimeout(onLoaded,10);return}
      clearInterval(c);
      emit({server:fmt(nav.responseEnd-nav.requestStart)});
      // Delay enabling RSC interception to skip hydration fetches
      setTimeout(function(){ ssrReady = true; }, 500);
    }
    if(document.readyState==='complete'){onLoaded()}
    else{window.addEventListener('load',onLoaded)}
  }
  setTimeout(updateSSR,0);

  // --- SPA navigation timing (RSC fetch) ---
  var origFetch=window.fetch;
  window.fetch=function(){
    var args=arguments;
    var opts=args[1]||{};
    var hdr=opts.headers||{};
    var get=function(h,k){return h?typeof h.get==='function'?h.get(k):h[k]:undefined};
    var isRSC=get(hdr,'rsc')==='1'||get(hdr,'next-router-state-tree');
    if(!isRSC&&args[0]&&typeof args[0]==='object'){
      var rHdr=args[0].headers||{};
      isRSC=get(rHdr,'rsc')==='1'||get(rHdr,'next-router-state-tree');
    }
    if(isRSC && ssrReady){
      var url=typeof args[0]==='string'?args[0]:args[0].href||args[0].url||'';
      emit({server:'--ms'});
      var base=url.split('?')[0];
      return origFetch.apply(this,args).then(function(res){
        var attempts=0;
        function poll(){
          attempts++;
          var entries=performance.getEntriesByType('resource');
          var match=null;
          for(var i=entries.length-1;i>=0;i--){
            if(entries[i].name.indexOf(base)!==-1&&entries[i].initiatorType==='fetch'&&entries[i].responseEnd>0){
              match=entries[i];break;
            }
          }
          if(match){
            emit({server:fmt(match.responseEnd-match.requestStart)});
          }else if(attempts<20){
            setTimeout(poll,50);
          }
        }
        setTimeout(poll,100);
        return res;
      });
    }
    return origFetch.apply(this,args);
  };
})();`;

export function TimingScript() {
  useEffect(() => {
    const script = document.createElement("script");
    script.textContent = TIMING_CODE;
    document.head.appendChild(script);
  }, []);

  return null;
}
