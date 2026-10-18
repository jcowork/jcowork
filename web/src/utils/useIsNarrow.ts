import { useEffect, useState } from 'react';

export function useIsNarrow(breakpoint = 768): boolean {
  const [isNarrow, setIsNarrow] = useState(() => window.innerWidth <= breakpoint);
  useEffect(() => {
    const onResize = () => setIsNarrow(window.innerWidth <= breakpoint);
    window.addEventListener('resize', onResize);
    return () => window.removeEventListener('resize', onResize);
  }, [breakpoint]);
  return isNarrow;
}
