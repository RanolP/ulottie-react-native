// lottie-web's types declare `export default Lottie`,
// but the package has no `exports` map or `"type": "module"`
import lottieWeb from 'lottie-web';

export const lottie = lottieWeb.default ?? lottieWeb;
export default lottie;
