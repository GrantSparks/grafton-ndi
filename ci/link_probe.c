/* Forces a final executable link with the selected vendor binary/import library. */
#include <Processing.NDI.Lib.h>
int main(void) {
    return NDIlib_version() == 0;
}
