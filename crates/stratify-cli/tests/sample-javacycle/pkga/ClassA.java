package pkga;

import pkgb.ClassB;

public class ClassA {
    public String aThing() {
        return new ClassB().bThing();
    }
}
